#!/usr/bin/env bash

mkdir -p $HOME/magpie-datasets/
pushd $HOME/magpie-datasets/

echo 'Getting undirected livejournal'
wget https://snap.stanford.edu/data/bigdata/communities/com-lj.ungraph.txt.gz
gunzip -c com-lj.ungraph.txt.gz > undirected-lj

echo 'Getting eu emails'
wget https://snap.stanford.edu/data/email-EuAll.txt.gz
gunzip -c email-EuAll.txt.gz > email-euall

echo 'Getting directed livejournal'
wget https://snap.stanford.edu/data/soc-LiveJournal1.txt.gz
gunzip -c soc-LiveJournal1.txt.gz > directed-lj

echo 'Getting twitter rv'
wget https://github.com/ANLAB-KAIST/traces/releases/download/twitter_rv.net/twitter_rv.net.00.gz
wget https://github.com/ANLAB-KAIST/traces/releases/download/twitter_rv.net/twitter_rv.net.01.gz
wget https://github.com/ANLAB-KAIST/traces/releases/download/twitter_rv.net/twitter_rv.net.02.gz
wget https://github.com/ANLAB-KAIST/traces/releases/download/twitter_rv.net/twitter_rv.net.03.gz

gunzip -c twitter_rv.net.00 twitter_rv.net.01 twitter_rv.net.02 twitter_rv.net.03 > twitter_rv.net

rm twitter_rv.net.*

popd
