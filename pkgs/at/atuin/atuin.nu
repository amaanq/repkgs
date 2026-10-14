use core.nu [cue]

# the server tests reach postgres://atuin:pass@localhost:5432/atuin unless ATUIN_DB_URI says otherwise
export def start-postgres []: nothing -> nothing {
   let data = $"($env.NIX_BUILD_TOP)/postgres"
   cue initdb --pgdata $data --username atuin --auth trust --no-locale --encoding UTF8
   cue pg_ctl start --wait --pgdata $data --log $"($data)/log" --options $"-h 127.0.0.1 -p 5432 -k ($data)"
   cue createdb --host 127.0.0.1 --port 5432 --username atuin atuin
}

export def stop-postgres []: nothing -> nothing {
   cue pg_ctl stop --wait --pgdata $"($env.NIX_BUILD_TOP)/postgres"
}
