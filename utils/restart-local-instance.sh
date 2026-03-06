#!/bin/bash

REDIS_ROOT=~/redis-stable/
# just to be safe
if [ -d "$REDIS_ROOT" ]; then
  pushd $REDIS_ROOT
  REDIS_PROCS=$(pgrep redis-server | wc -l)
  if [ "$REDIS_PROCS" -ne 0 ]; then
    ./src/redis-cli shutdown
  fi
  popd
fi

pkill memcached
pkill magpie
pushd $MAGPIE_ROOT

# increase fd limit for worklads with many allocations
ulimit -n 262144

export LD_LIBRARY_PATH="$(rustc --print sysroot)/lib":$PWD/../applications/$MAGPIE_APPLICATION/target/release
MAGPIE_IGNORE_CACHEABLE=1 RUST_BACKTRACE=1 target/release/magpie -c $MAGPIE_CONFIG -r >/tmp/magpie-latest-logs 2>&1 &
popd
