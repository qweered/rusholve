#!/usr/bin/env bash
# Hard stop: `select` is unsupported by brush_parser.
select choice in alpha beta gamma; do
  echo "$choice"
  break
done
