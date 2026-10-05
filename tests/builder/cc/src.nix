{
  "data/file" = ''
    x
  '';
  "lib.c" = ''
    int hello(void) { return 1; }
  '';
  "main.c" = ''
    #include <stdio.h>
    int hello(void);
    int main(void) { printf("%d\n", hello()); return 0; }
  '';
}
