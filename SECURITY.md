# Security

## Threat model

Scriptorium runs LaTeX on the machine hosting it. Anyone who can reach the
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

## Reporting a vulnerability

Please report vulnerabilities privately to the maintainers rather than opening a
public issue, and allow reasonable time for a fix.
