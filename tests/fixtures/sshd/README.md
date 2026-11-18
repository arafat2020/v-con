# sshd fixture

Docker Compose OpenSSH server used by the ignored tests in
`tests/integration_ssh.rs`.

```bash
DOCKER=1 cargo test --test integration_ssh -- --ignored
```

Those tests generate an ed25519 key, export the public key as
`VCON_TEST_PUBLIC_KEY`, and start this stack themselves on
`127.0.0.1:2222` as user `test`. They then run:

- `vcon exec` and check the exit status and command output
- `vcon status --no-cpu` and check the host is reachable

`cargo test` without `--ignored` does not start Docker. CI runs this
suite only when `docker info` succeeds.

To bring the server up by hand, pass the same public key:

```bash
export VCON_TEST_PUBLIC_KEY="$(cat ~/.ssh/id_ed25519.pub)"
docker compose -f tests/fixtures/sshd/docker-compose.yml up -d
docker compose -f tests/fixtures/sshd/docker-compose.yml down -v
```

Pubkey authentication is required. Password login is off.
