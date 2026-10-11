#!/bin/sh
# bro eval: run each case in evals/cases/ and check the command against its patterns.
#
#   evals/run.sh [--endpoint URL] [--model NAME] [--tool bro|apfel-cmd|butterfish] [--bro PATH]
#                [--pause SECONDS] [case ...]
#
# A case is a context snapshot (bro's --context-file format, merged over evals/base.json) plus:
#   "accept":    regexes (extended); the command must match at least one
#   "reject":    regexes; the command must match none
#   "dangerous": true/false; bro's danger flag must agree (bro only)
#
# Baselines get only the request text (they can't use bro's context):
#   apfel-cmd   apfel's bundled `cmd` demo: apfel -q --code with its system prompt
#   butterfish  `butterfish gencmd`, pointed at --endpoint (needs evals/model-proxy.py for apfel)
#
# --pause waits between cases, for rate-limited cloud endpoints (Gemini's free tier); a case
# that gets a 429 or 503 is retried up to three times.
#
# Results are appended to evals/results/<tool>.tsv so they can be compared over time.
# Needs jq and perl (both ship with macOS 15+).

set -u
here=$(cd "$(dirname "$0")" && pwd)
endpoint="" model="" tool=bro bro="$here/../target/release/bro" pause=0
while [ $# -gt 0 ]; do
  case $1 in
    --endpoint) endpoint=$2; shift 2 ;;
    --model) model=$2; shift 2 ;;
    --tool) tool=$2; shift 2 ;;
    --bro) bro=$2; shift 2 ;;
    --pause) pause=$2; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    -*) echo "unknown option $1" >&2; exit 2 ;;
    *) break ;;
  esac
done
if [ $# -eq 0 ]; then
  set -- "$here"/cases/*.json
else
  for c in "$@"; do shift; set -- "$@" "$here/cases/${c%.json}.json"; done
fi

now_ms() { perl -MTime::HiRes=time -e 'printf "%.0f\n", time*1000'; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$here/results"
stamp=$(date +%Y-%m-%dT%H:%M:%S)
label="$tool${model:+ $model}${endpoint:+ $endpoint}"
pass=0 total=0 times=""

apfel_cmd_system="You are a shell command generator for macOS (zsh/bash). Output ONLY the command — no explanation, no markdown, no code fences, no comments. If multiple commands are needed, join them with && or |. Never output anything except the command itself."

first=1
for case in "$@"; do
  [ "$first" = 1 ] || sleep "$pause"
  first=0
  name=$(basename "$case" .json)
  jq -s '.[0] * .[1]' "$here/base.json" "$case" > "$work/context.json"
  request=$(jq -r '.request // ""' "$work/context.json")
  line=$(jq -r '.line // ""' "$work/context.json")
  plain="$request"
  [ -n "$line" ] && plain="finish this command: $line${request:+ ($request)}"

  start=$(now_ms)
  dangerous=""
  case $tool in
    bro)
      # A rate limit (429) or overload (503) says nothing about the answer: wait and ask again,
      # timing only the attempt that got through.
      for attempt in 1 2 3 4; do
        start=$(now_ms)
        out=$("$bro" --json --context-file "$work/context.json" ${endpoint:+--endpoint "$endpoint"} ${model:+--model "$model"} </dev/null 2>/dev/null)
        command=$(printf '%s' "$out" | jq -r '.command // ("ERROR: " + (.error // "no output"))' 2>/dev/null || echo "ERROR: $out")
        case $command in
          "ERROR: the model server answered 429"*|"ERROR: the model server answered 503"*) sleep $((attempt * 20)) ;;
          *) break ;;
        esac
      done
      dangerous=$(printf '%s' "$out" | jq -r '.dangerous // false' 2>/dev/null)
      ;;
    apfel-cmd)
      command=$(apfel -q --code -s "$apfel_cmd_system" "$plain" 2>/dev/null | sed 's/^[[:space:]]*//' | sed '/^$/d')
      ;;
    butterfish)
      command=$(OPENAI_API_KEY=${OPENAI_API_KEY:-unused} NO_COLOR=1 butterfish ${endpoint:+-u "$endpoint/responses"} --service-tier= \
        gencmd -r "" "$plain" </dev/null 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' | sed '/^[[:space:]]*$/d')
      ;;
    *) echo "unknown tool $tool" >&2; exit 2 ;;
  esac
  took=$(( $(now_ms) - start ))
  times="$times $took"
  flat=$(printf '%s' "$command" | awk 'NR > 1 { printf " ; " } { printf "%s", $0 }')

  ok=0
  for re in $(jq -r '(.accept // [])[] | @base64' "$work/context.json"); do
    re=$(printf '%s' "$re" | base64 -d)
    printf '%s\n' "$flat" | grep -Eq -- "$re" && ok=1
  done
  why=""
  [ $ok -eq 0 ] && why="no accepted pattern"
  for re in $(jq -r '(.reject // [])[] | @base64' "$work/context.json"); do
    re=$(printf '%s' "$re" | base64 -d)
    if printf '%s\n' "$flat" | grep -Eq -- "$re"; then ok=0; why="matches rejected /$re/"; fi
  done
  want=$(jq -r 'if has("dangerous") then (.dangerous|tostring) else "" end' "$work/context.json")
  if [ "$tool" = bro ] && [ -n "$want" ] && [ "$want" != "$dangerous" ]; then ok=0; why="danger flag $dangerous, expected $want"; fi

  total=$((total + 1))
  if [ $ok -eq 1 ]; then pass=$((pass + 1)); verdict=PASS; else verdict=FAIL; fi
  printf '%s %6s ms  %-22s %s%s\n' "$verdict" "$took" "$name" "$flat" "${why:+   ($why)}"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$stamp" "$label" "$name" "$verdict" "$took" "$flat" >> "$here/results/$tool.tsv"
done

median=$(printf '%s\n' $times | sort -n | awk '{a[NR]=$1} END {print (NR%2 ? a[(NR+1)/2] : int((a[NR/2]+a[NR/2+1])/2))}')
echo "$label: $pass of $total passed; median $median ms per request"
