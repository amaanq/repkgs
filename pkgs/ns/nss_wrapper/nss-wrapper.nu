# the sandbox has no /etc/services
export def services []: nothing -> nothing {
   cp $"($env.IANA_ETC)/services" /etc/services
}
