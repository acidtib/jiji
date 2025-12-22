# Jiji Documentation

This directory contains detailed documentation for developers and contributors
working with Jiji.

## Contents

### Configuration

**[jiji.yml Example](../src/jiji.yml)** - Comprehensive example configuration
file with all available options and detailed comments

### Networking

**[Network Reference](network-reference.md)** - Quick reference guide for Jiji's
private networking features including WireGuard mesh, automatic service
discovery via DNS, and daemon-level DNS configuration

### Logging

**[Logs Reference](logs-reference.md)** - Comprehensive guide for viewing and
filtering logs from services and kamal-proxy, including real-time following,
grep filtering, and common debugging workflows

### Registry

**[Registry Auto-Detection](registry-auto-detection.md)** - Guide for automatic
namespace detection and authentication with container registries (GHCR, Docker
Hub, local registries)

### Development

**[Testing Guide](testing.md)** - Instructions for testing Jiji deployments
including proxy testing, SSH connections, and troubleshooting
**[Version Script](version.md)** - Documentation for the version management
utility and `jiji version` command

## Quick Links

### For Users

[Main README](../README.md) - Installation, usage, and getting started guide
[Configuration Reference](../src/jiji.yml) - Complete configuration options

### For Contributors

[Testing Guide](testing.md) - How to test your changes
[Network Architecture](network-reference.md) - Understanding Jiji's networking
layer and DNS service discovery [Logs Reference](logs-reference.md) - Debugging
with logs and monitoring services

## Getting Help

[Issues](https://github.com/acidtib/jiji/issues) - Report bugs or request
features [Discussions](https://github.com/acidtib/jiji/discussions) - Ask
questions and share ideas
