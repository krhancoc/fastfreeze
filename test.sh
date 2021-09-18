#/bin/sh
p=$(pwd)
cd tests/checkpoint
cargo b
cd $p

cargo b

LD_LIBRARY_PATH=$LD_LIBRARY_PATH:$(pwd)/dist/lib
export LD_LIBRARY_PATH

PATH=$PATH:$(pwd)/dist/bin
export PATH
export FF_METRICS_RECORDER=echo

echo $LD_LIBRARY_PATH
echo $PATH

./target/debug/fastfreeze run -v -n test ./tests/checkpoint/target/debug/checkpoint
