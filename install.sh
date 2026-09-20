#!/bin/sh

git clone https://github.com/Smithay/smithay
cd smithay || return
git checkout 5fb12b87407b3680135c45d94214c5f1b1d0fbea
git apply ../openbsd/smithay-0.7-openbsd.patch
cargo build --release
