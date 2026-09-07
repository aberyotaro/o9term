# Loaded last in .zshrc. Only enable inside 0s9terminal.
[[ -o interactive && ${TERM_PROGRAM-} == os9terminal ]] || return 0

# Release packages include bat and its libraries.
if [[ -n ${OS9_RESOURCES-} && -x $OS9_RESOURCES/bin/bat ]]; then
    typeset -U path
    path=("$OS9_RESOURCES/bin" $path)
fi

# bat's ANSI theme follows the terminal's 16-color palette.
export BAT_THEME=${BAT_THEME:-ansi}

# Avoid registering the same ZLE hooks twice.
if (( ! ${+functions[_zsh_highlight]} )); then
    for _os9_highlight in "${OS9_RESOURCES:-/nonexistent}/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh" /opt/homebrew/share/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh /usr/local/share/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh; do
        if [[ -r $_os9_highlight ]]; then
            source "$_os9_highlight"
            break
        fi
    done
    unset _os9_highlight
fi
