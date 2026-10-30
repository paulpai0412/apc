#!/usr/bin/env fish
# System toolchains are not installed. Dependency downloads require explicit opt-in.
set -l here (path dirname (status filename))
set -l repository (path resolve "$here/../..")
if not command -q python3
    echo 'Missing prerequisite: python3 (Python 3.11+). No installer was run.' >&2
    exit 2
end
python3 -B "$repository/validation/cement_workspace/workspace.py" $argv
exit $status
