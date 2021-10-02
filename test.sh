#/bin/sh
date +"%T"
echo $$
sleep 5
echo "Nothing" | nc -w 1 -U /var/tmp/fastfreeze/run/fastfreeze.sock
sleep 30
date +"%T"
