# filename escaping test
NAME="$(printf "/tmp/test$1.sh")"
echo '#!'"$(which bash)"'
PID=$BASHPID
cat /proc/$PID/status | grep -a Name | hexdump -C' >"$NAME"
chmod +x "$NAME"
"$NAME"
