{
  "go.mod" = ''
    module example.test/hello

    go 1.22
  '';
  "main.go" = ''
    package main

    import "fmt"

    func main() { fmt.Println(1) }
  '';
}
