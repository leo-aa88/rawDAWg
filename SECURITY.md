# Security Policy

## Supported versions

Only the latest commit on `main` and the latest published release are supported. There are no
maintained release branches.

## Reporting a vulnerability

Please **do not open a public issue** for security problems. Report them privately through
[GitHub private vulnerability reporting](https://github.com/leo-aa88/rawDAWg/security/advisories/new).
Include the affected version or commit (`rawdawg --version`), the steps or file that reproduce it,
and the impact you expect. You should get an acknowledgement within a week.

## What counts as a security issue

rawDAWg opens files from other people (projects, `.rawdawgpreset` presets, audio, MIDI and
SoundFont files) and loads native plugins, so these are treated as security issues:

- a crafted project, preset, audio, MIDI or SoundFont file that crashes rawDAWg, hangs it, makes
  it allocate without bound, or reads or writes files outside the project folder (path traversal,
  symlinks, archive extraction);
- a crafted file that panics or blocks the real-time audio thread;
- a plugin module that rawDAWg loads into its own process without first passing the isolated
  scanner (a scanner bypass), or that runs after being quarantined;
- memory-safety problems in rawDAWg's own `unsafe` code (plugin hosting and export, native
  windows, FFI);
- release packages that do not match the published checksums or the tagged source.

Not security issues:

- a crash or hang **inside** a third-party plugin while it runs in-process. Plugins are native
  code that runs with your rights; rawDAWg contains discovery (separate scanner process,
  quarantine, safe mode) but cannot contain a plugin's own crash during processing. This is
  documented in `docs/PLUGIN_API.md` and the user guide;
- audio dropouts, wrong sound or other bugs without a security impact (use a bug report).

Releases are unsigned until the maintainer decides on signing; verify downloads with the
`SHA256SUMS.txt` attached to each release.
