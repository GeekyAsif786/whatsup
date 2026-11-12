# Demo GIF for the README. Record with VHS (https://github.com/charmbracelet/vhs):
#
#   vhs docs/demo.tape          # writes docs/demo.gif
#
# Needs `whatsup` on PATH (cargo install --path .) and python3. The hidden
# setup starts two throwaway servers so the demo has something to show: one
# local-only, one exposed to the network. Your own dev servers appear too;
# stop the ones you don't want in the recording first. Upload the GIF to a
# GitHub issue/PR comment and put that URL in README.md at <!-- DEMO -->.

Output docs/demo.gif

Require whatsup
Require python3

Set Shell "bash"
Set FontSize 16
Set Width 1200
Set Height 640
Set Padding 16
Set TypingSpeed 60ms
Set Theme "Catppuccin Mocha"

Hide
Type "export PS1='$ '; cd \"$(mktemp -d)\""
Enter
Type "python3 -m http.server 5500 --bind 127.0.0.1 >/dev/null 2>&1 & python3 -m http.server 8000 --bind 0.0.0.0 >/dev/null 2>&1 &"
Enter
Sleep 1s
Type "clear"
Enter
Show

# The one-shot list.
Type "whatsup list"
Sleep 400ms
Enter
Sleep 3s

# The interactive view.
Type "whatsup"
Sleep 400ms
Enter
Sleep 3s
Down
Sleep 1s
Down
Sleep 1.5s

# Filter to the exposed server.
Type "/8000"
Sleep 1s
Enter
Sleep 2s

# Stop it: x opens the prompt, y confirms.
Type "x"
Sleep 2s
Type "y"
Sleep 4s

# Clear the filter and quit.
Type "/"
Sleep 300ms
Escape
Sleep 1.5s
Type "q"
Sleep 1s

Type "whatsup port 8000; echo exit=$?"
Sleep 400ms
Enter
Sleep 3s

Hide
Type "kill %1 %2 2>/dev/null; clear"
Enter
