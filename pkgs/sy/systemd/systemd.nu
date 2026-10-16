# the journal tests skip themselves on a machine without an id
export def machine-id []: nothing -> nothing {
   "0123456789abcdef0123456789abcdef\n" | save -f /etc/machine-id
}
