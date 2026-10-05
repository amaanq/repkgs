{
  "cli.js" = ''
    #!/usr/bin/env node
    console.log("test-npm " + require("./package.json").version);
  '';
  "package-lock.json" = ''
    {
      "name": "test-npm",
      "version": "2.0.0",
      "lockfileVersion": 3,
      "requires": true,
      "packages": {
        "": {
          "name": "test-npm",
          "version": "2.0.0",
          "bin": {
            "test-npm": "cli.js"
          }
        }
      }
    }
  '';
  "package.json" = ''
    {
      "name": "test-npm",
      "version": "2.0.0",
      "bin": {
        "test-npm": "cli.js"
      },
      "scripts": {
        "test": "node cli.js | grep -q 'test-npm 2.0.0'"
      }
    }
  '';
}
