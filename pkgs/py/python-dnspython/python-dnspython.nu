# the WKS rdata tests look up protocols and services by name
export def etc []: nothing -> nothing {
   cp $"($env.IANA_ETC)/protocols" $"($env.IANA_ETC)/services" /etc
}
