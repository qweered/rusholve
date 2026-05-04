#!/usr/bin/env bash
# Hard stop: `coproc` is unsupported by brush_parser.
coproc tail -F /var/log/syslog
