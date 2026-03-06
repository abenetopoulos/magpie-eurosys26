#!/bin/bash

REDIS_ROOT=~/redis-stable/
REDIS_CONFIG=$REDIS_ROOT/redis.conf

# just to be safe
pkill memcached
pkill magpie
# need to wait for the port to become available, this should be an OK default
pushd $REDIS_ROOT
REDIS_PROCS=$(pgrep redis-server | wc -l)
if [ "$REDIS_PROCS" -ne 0 ]; then
  >&2 echo "Num procs $REDIS_PROCS"
  ./src/redis-cli shutdown
fi
rm -f dump.rdb
echo 'Starting redis server'
./src/redis-server $REDIS_CONFIG --daemonize yes
popd
