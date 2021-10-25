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

echo $LD_LIBRARY_PATH
echo $PATH

./target/debug/fastfreeze run -vv -n test ./tests/checkpoint/target/debug/checkpoint
