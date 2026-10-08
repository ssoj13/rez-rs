# Caching and Memcached

rez-rs supports multiple caching layers to speed up repeated operations. This chapter explains how caching works and how to set up Memcached for shared caching across processes and machines.

## Overview

| Layer | Purpose | When used |
|-------|---------|-----------|
| **Parsed package cache** | Cache parsed `package.py`/yaml/toml → JSON | Always (disk: `~/.rez/parsed_package_cache/`) |
| **FsRepoCached** | In-memory cache of families, versions, package data | When `cache_listdir` or `cache_package_files` is true and memcached is *not* configured |
| **Memcached** | Shared cache across processes/machines | When `memcached_uri` is non-empty |

By default, `memcached_uri` is empty, so rez-rs uses local caches only. Configure Memcached when you need shared caching (e.g. render farms, multi-user studios).

## Memcached: What and Why

**Memcached** is an in-memory key-value store. rez-rs uses it for:

- Directory listings (family names, version lists) — via FsRepoMemcached when memcached_uri set
- Parsed package file contents — via FsRepoMemcached when memcached_uri set
- `rez memcache` CLI: flush, stats, warm, poll

**Benefits:**

- **Shared across processes** — multiple `rez env` invocations share the same cache
- **Shared across machines** — all nodes in a farm hit the same cache server
- **Faster repeats** — second `rez env maya` on the same machine (or another node) can avoid re-reading disks and re-parsing package files

**Without Memcached:** Each rez process uses its own in-memory cache (FsRepoCached) or reads from disk. Caches are not shared.

## Configuration

### Enable Memcached

Set `memcached_uri` in your rezconfig:

```python
# rezconfig.py
memcached_uri = ["127.0.0.1:11211"]
```

For multiple servers (redundancy / sharding):

```python
memcached_uri = ["memcache01:11211", "memcache02:11211"]
```

Or via environment:

```bash
export REZ_MEMCACHED_URI_JSON='["127.0.0.1:11211"]'
```

### Related config

| Field | Default | Description |
|-------|---------|-------------|
| `memcached_uri` | `[]` | Server URIs; empty = disabled |
| `cache_listdir` | `true` | Cache directory listings |
| `cache_package_files` | `true` | Cache parsed package contents |

When `memcached_uri` is empty, `rez memcache` will exit with "memcaching is not enabled."

## Docker: Local Memcached

For local development or testing:

```bash
# Start Memcached
docker run -d --name rez-memcached -p 11211:11211 memcached:1.6-alpine

# Verify
docker ps --filter name=rez-memcached

# Stop
docker stop rez-memcached

# Remove
docker rm -f rez-memcached
```

Then in `~/.rez/rezconfig.py`:

```python
memcached_uri = ["127.0.0.1:11211"]
```

## rez memcache Command

Manage and inspect Memcached servers:

```bash
# Default: show summary (servers, uptime, hits, misses, hit ratio)
rez memcache

# Detailed stats per server
rez memcache --stats

# Flush all cached data
rez memcache --flush

# Warm cache (scan packages_path)
rez memcache --warm
rez memcache --warm -v   # verbose

# Poll (placeholder; not fully implemented)
rez memcache --poll
```

**Without memcached configured:**

```bash
$ rez memcache
memcaching is not enabled.
Set 'memcached_uri' in rezconfig to enable.
# exit code 1
```

## How It Works

1. **Config** — If `memcached_uri` is non-empty, rez-rs creates a `MemcacheClient` on first use.

2. **Connect** — The client connects to the first server in the list when the first cache operation runs (lazy connect).

3. **Keys** — Cache keys follow patterns like:
   - `rez:pkg:{repo_hash}:{name}:{version}` — package data
   - `rez:listdir:{repo_hash}` — directory listing
   - (Future) resolve keys

4. **TTL** — Cached items have a time-to-live. Stale data is evicted by Memcached automatically.

5. **Disabled** — When `memcached_uri` is empty:
   - `MemcacheClient::get` returns `None`
   - `set`, `flush`, `delete` are no-ops
   - `rez memcache` exits with an error message

## Testing With and Without Memcached

**With Memcached (Docker running, `memcached_uri` set):**

```bash
rez memcache              # Shows summary
rez memcache --stats      # Detailed stats
rez memcache --flush      # Clears cache
```

**Without Memcached (`memcached_uri` empty):**

```bash
rez memcache
# memcaching is not enabled.
# Set 'memcached_uri' in rezconfig to enable.
# exit 1
```

Unit tests cover both: `test_memcache_client_disabled` (empty servers) and `test_memcache_client_enabled` (with a server).
