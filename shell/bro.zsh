# bro zsh integration, printed by `bro init zsh`. Load it with:  eval "$(bro init zsh)"
#
# Enter on a line starting with `bro` (any case, optional punctuation: `Bro,` `bro.` `BRO:`)
# sends the rest of the line, verbatim, to bro instead of zsh, so apostrophes, `?` and `!` are
# fine. The line stays on screen and goes into your history as typed, where later requests can
# see it; it isn't run. The answer fills a fresh prompt under it; nothing runs until you press
# Enter again.
# __BRO_KEY__ sends whatever is typed to bro to finish it.
#
# `cat file | bro <request>` runs bro as an ordinary command: it shows the piped text (like tee)
# and leaves its answer in a file named after this shell, which is put in the next prompt.

[[ -o interactive ]] || return 0
(( ${+_bro_loaded} )) && return 0
typeset -g _bro_loaded=1
zmodload zsh/parameter 2>/dev/null

typeset -g _bro_bin=__BRO_BIN__
typeset -g _bro_status=0       # exit status of the last command
typeset -g _bro_waiting=0      # 1: a suggestion is in the prompt; 2: the next command ran
typeset -g _bro_req= _bro_sug= _bro_exp= _bro_ran=
typeset -g _bro_pending=__BRO_PENDING__/$$
# Which terminal session piped output belongs to, and where bro leaves answers for this shell.
export BRO_SESSION=$$

# Record what was run after a suggestion, and its exit status (learning). In the background.
_bro_precmd() {
  local st=$?
  _bro_status=$st
  if (( _bro_waiting == 2 )); then
    BRO_REC_REQUEST=$_bro_req BRO_REC_SUGGESTED=$_bro_sug BRO_REC_EXPLANATION=$_bro_exp BRO_REC_RAN=$_bro_ran BRO_REC_STATUS=$st \
      $_bro_bin --record &!
    _bro_waiting=0
  fi
}
_bro_preexec() {
  # So `… | bro` knows which command's output it's reading.
  if [[ $1 == *'|'*[Bb][Rr][Oo]* ]]; then
    export BRO_COMMAND_LINE=$1
  else
    unset BRO_COMMAND_LINE
  fi
  if (( _bro_waiting == 1 )); then
    _bro_ran=$1
    _bro_waiting=2
  fi
}
# Run first, so $? is still the command's status.
precmd_functions=(_bro_precmd ${precmd_functions:#_bro_precmd})
preexec_functions=(_bro_preexec ${preexec_functions:#_bro_preexec})

# A suggestion that was replaced by a new request: record it as not run.
_bro_unused() {
  if (( _bro_waiting == 1 )); then
    BRO_REC_REQUEST=$_bro_req BRO_REC_SUGGESTED=$_bro_sug BRO_REC_EXPLANATION=$_bro_exp $_bro_bin --record &!
    _bro_waiting=0
  fi
}

# The explanation, under the prompt; red for a dangerous command.
_bro_show() {
  local text=$1 colour=$2
  POSTDISPLAY=$'\n'"  $text"
  region_highlight=(${region_highlight:#*memo=bro})
  local start=$(( ${#BUFFER} + 1 )) end=$(( ${#BUFFER} + ${#POSTDISPLAY} ))
  region_highlight+=("P$start $end fg=$colour memo=bro")
}
_bro_clear() {
  POSTDISPLAY=
  region_highlight=(${region_highlight:#*memo=bro})
}
# A new prompt: clear the old explanation, and put in an answer left by `… | bro <request>`.
_bro_line_init() {
  _bro_clear
  [[ -f $_bro_pending ]] || return 0
  local -a rec
  rec=("${(@f)$(<$_bro_pending)}")
  command rm -f -- $_bro_pending
  local US=$'\x1f'
  local st=$rec[1] command=${rec[2]//$US/$'\n'} explanation=${rec[3]//$US/ }
  [[ -n $command ]] || return 0
  _bro_unused
  BUFFER=$command CURSOR=$#BUFFER
  _bro_req=${rec[4]//$US/ } _bro_sug=$command _bro_exp=$explanation _bro_waiting=1 _bro_shown=$command
  if [[ $st == danger ]]; then
    _bro_show "$explanation" red
  elif [[ -n $explanation ]]; then
    _bro_show "bro: $explanation" 8
  fi
}

# The explanation describes the suggestion; once the line is edited, it no longer applies.
typeset -g _bro_shown=
_bro_redraw() {
  if [[ -n $POSTDISPLAY && -n $_bro_shown && $BUFFER != $_bro_shown ]]; then
    _bro_clear
    _bro_shown=
  fi
}

# Ask bro. $1: the request (may be empty), $2: the current line for completion (may be empty),
# $3: the line to put back if bro fails or you press Ctrl-C (default: what's in the prompt now).
# bro streams its answer one record per line (fields split by a tab, newlines inside a field sent
# as 0x1f): `pid` first, `partial` while the command is being written, `command ok|danger` once
# it's done, then `explanation`, or `error`. The prompt fills as the command arrives. Ctrl-C stops
# bro and puts back the line as it was (zle would otherwise throw the line away).
_bro_ask() {
  emulate -L zsh
  local request=$1 line=$2 RS=$'\x1e' US=$'\x1f' TAB=$'\t'
  local -a recent aliases_kv
  local k v
  for k v in ${(kv)aliases}; do aliases_kv+=("$k=$v"); done
  for k in ${(Onk)history}; do
    [[ -n ${history[$k]//[[:space:]]/} ]] || continue
    recent=("${history[$k]}" $recent)
    (( $#recent >= __BRO_RECENT__ )) && break
  done
  local original=${3-$BUFFER} command= st= explanation= error= record kind rest fd pid= cancelled=0
  setopt local_traps
  trap 'cancelled=1' INT
  _bro_show "bro is cooking… (Ctrl-C to cancel)" 8
  zle -R
  local -a extra
  [[ -n $line ]] && extra=(--line "$line")
  {
    exec {fd}< <(BRO_HISTORY=${(pj:$RS:)recent} BRO_STATUS=$_bro_status \
        BRO_ALIASES=${(pj:$RS:)aliases_kv} BRO_FUNCTIONS="${(k)functions:#_*}" \
        BRO_JOBS=${(pj:$RS:)jobtexts} \
        $_bro_bin --zle $extra <<<"$request" 2>/dev/null)
    while IFS= read -r -u $fd record; do
      kind=${record%%$TAB*} rest=${record#*$TAB}
      (( cancelled )) && break
      case $kind in
        pid) pid=$rest ;;
        partial)
          BUFFER=${rest//$US/$'\n'} CURSOR=$#BUFFER
          _bro_show "bro is cooking… (Ctrl-C to cancel)" 8
          zle -R ;;
        command)
          st=${rest%%$TAB*} command=${${rest#*$TAB}//$US/$'\n'}
          BUFFER=$command CURSOR=$#BUFFER
          [[ $st == danger ]] && _bro_show "⚠ checking…" red || _bro_show "bro: …" 8
          zle -R ;;
        explanation) explanation=${rest//$US/ } ;;
        error) error=${rest//$US/ } ;;
      esac
    done
  } always {
    [[ -n $fd ]] && exec {fd}<&-
    if (( cancelled )); then
      [[ -n $pid ]] && kill $pid 2>/dev/null
      command=
    fi
    # Cancelled or failed: put back what was there.
    [[ -n $command ]] || { BUFFER=$original CURSOR=$#BUFFER; }
  }
  if [[ -n $command ]]; then
    _bro_req=${request:-$line} _bro_sug=$command _bro_exp=$explanation _bro_waiting=1 _bro_shown=$command
    if [[ $st == danger ]]; then
      _bro_show "$explanation" red
    elif [[ -n $explanation ]]; then
      _bro_show "bro: $explanation" 8
    else
      _bro_clear
    fi
  elif [[ -n $error ]]; then
    _bro_show "$error" yellow
  elif (( cancelled )); then
    _bro_show "bro: cancelled" 8
  else
    _bro_show "bro: no answer" yellow
  fi
}

_bro_accept_line() {
  emulate -L zsh
  setopt extended_glob
  _bro_clear
  if [[ -z $PREBUFFER && $BUFFER =~ '^[[:space:]]*[Bb][Rr][Oo][[:punct:]]*([[:space:]]|$)' ]]; then
    local rest=${BUFFER[$((MEND + 1)),-1]}
    local words=${${rest##[[:space:]]#}%%[[:space:]]#}
    # `bro` alone, its subcommands with nothing after them, and flags run as normal commands.
    if [[ -z $words || $words == (install|config|init\ zsh) || $words == -* ]]; then
      zle _bro_orig_accept_line
      return
    fi
    _bro_unused
    # Into the history as typed, and left on screen: `zle -I` moves on to a fresh prompt under
    # the line, where the answer appears. Nothing is run, so $? still belongs to the last
    # command ("fix it").
    local typed=${BUFFER##[[:space:]]#}
    print -rs -- "$typed"
    zle -I
    BUFFER= CURSOR=0
    _bro_ask "$rest" "" "$typed"
    return
  fi
  zle _bro_orig_accept_line
}

_bro_complete() {
  emulate -L zsh
  [[ -n ${BUFFER//[[:space:]]/} ]] || return 0
  _bro_unused
  _bro_ask "" "$BUFFER"
}

zle -A accept-line _bro_orig_accept_line
zle -N accept-line _bro_accept_line
zle -N _bro_complete
bindkey -M emacs '__BRO_KEY__' _bro_complete
bindkey -M viins '__BRO_KEY__' _bro_complete
autoload -Uz add-zle-hook-widget && {
  add-zle-hook-widget line-init _bro_line_init
  add-zle-hook-widget line-pre-redraw _bro_redraw
}
