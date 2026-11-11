# Security

## Threat model

Chrysanthemum runs LaTeX on the machine hosting it. Anyone who can reach the
server and authenticate can create and edit files and compile documents there.
Shell escape is disabled and compilations time out, but TeX remains a powerful
language. Only share access with people you trust.

- The server binds to `127.0.0.1` by default and rejects unexpected `Host`
  headers (DNS-rebinding defence).
- With `--lan`, a random access token is required unless `--no-auth` is passed.
  The token travels in the join link and is stored in a `SameSite=Strict`
  cookie. Traffic is plain HTTP; use a trusted network or a TLS-terminating
  reverse proxy.
- State-changing requests and WebSockets with a foreign `Origin` are refused.
- Installing or removing compilers is limited to requests that come from a loopback address, so collaborators who join with the access token cannot change what is installed on the host. If a reverse proxy runs on the same machine, every request appears local; do not expose the server through one without your own access control.
- Compilers are downloaded only from fixed `https://github.com/` release URLs compiled into the program, and each archive is checked against an embedded SHA-256 digest before it is unpacked. Archives are unpacked into a staging directory that refuses paths leaving it, and a compiler is moved into place only after it runs its own `--version` self-test. Certificates are checked against the operating system's trust store.

## Reporting a vulnerability

Please report vulnerabilities privately to the maintainers rather than opening a
public issue, and allow reasonable time for a fix.
