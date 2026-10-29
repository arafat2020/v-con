# sshd fixture

Docker Compose definition for Phase 9 integration-test scaffolding.

```bash
docker compose -f tests/fixtures/sshd/docker-compose.yml up -d
DOCKER=1 cargo test --test integration_ssh -- --ignored
docker compose -f tests/fixtures/sshd/docker-compose.yml down -v
```

The current suite only asserts the fixture files exist. Extend with a
generated key, bastion jump host, and real `vcon exec` / SFTP / tunnel cases
when hardening the multi-container CI job.
