use core.nu [x]

# the server tests reach postgres://atuin:pass@localhost:5432/atuin unless ATUIN_DB_URI says otherwise
export def start-postgres []: nothing -> nothing {
   let data = $"($env.NIX_BUILD_TOP)/postgres"
   x initdb --pgdata $data --username atuin --auth trust --no-locale --encoding UTF8
   x pg_ctl start --wait --pgdata $data --log $"($data)/log" --options $"-h 127.0.0.1 -p 5432 -k ($data)"
   x createdb --host 127.0.0.1 --port 5432 --username atuin atuin
}

export def stop-postgres []: nothing -> nothing {
   x pg_ctl stop --wait --pgdata $"($env.NIX_BUILD_TOP)/postgres"
}
