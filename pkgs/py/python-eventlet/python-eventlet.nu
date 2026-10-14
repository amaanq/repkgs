export def etc []: nothing -> nothing {
   "nameserver 127.0.0.1\n" | save -f /etc/resolv.conf
   cp $"($env.IANA_ETC)/protocols" $"($env.IANA_ETC)/services" /etc
}
