#!/usr/bin/env bash
# Hard stop: `disown` is unsupported by brush_parser.
sleep 100 &
disown -a
