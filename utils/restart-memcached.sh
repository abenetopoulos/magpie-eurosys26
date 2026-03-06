#!/usr/bin/env bash

# just to be safe
REDIS_ROOT=~/redis-stable/
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

sleep 10
memcached --daemon -m 1024 -t 16 -l 0.0.0.0
