#!/bin/sh
# cargo's runner (.cargo/config.toml): write the bought form of the ELF (bought.py) over BOOTSEL.
set -e
python3 "$(dirname "$0")/bought.py" "$1" "$1.bought"
picotool load -u -v -x -t elf "$1.bought"
