# whatsup-cli

Every port, dev server, database and container on your machine in one terminal view: see what
it is, whether the network can reach it, and stop it.

```sh
npx whatsup-cli            # interactive view
npx whatsup-cli list       # print the list
npm install -g whatsup-cli # then just: whatsup
```

This package is a small launcher. npm installs the native binary for your platform from one of
its optional dependencies (`@whatsup-cli/darwin-arm64`, `darwin-x64`, `linux-x64`,
`linux-arm64`, `win32-x64`); nothing is downloaded by an install script. If you install with
`--omit=optional`, the binary is skipped and `whatsup` tells you so.

Documentation, other install methods and source: https://github.com/GeekyAsif786/whatsup
