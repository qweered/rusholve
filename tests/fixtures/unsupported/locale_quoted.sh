#!/usr/bin/env bash
# Hard stop: locale-aware $"…" quoting (gettext form). brush parses it
# but its semantics under our resolver are unspecified.
echo $"please translate me"
