# Jiji Documentation

This directory contains documentation for developers, LLMs and contributors
working with Jiji.

## Contents

### Getting Started

**[Deployment Guide](deployment-guide.md)** - Complete guide to deploying
applications with Jiji, from initial setup to CI/CD integration and advanced
deployment patterns

**[Configuration Reference](configuration-reference.md)** - Complete reference
for all configuration options including builder, SSH, network, registry, and
service configuration

### Configuration

**[jiji.yml Example](../src/jiji.yml)** - Example configuration file with all
available options and detailed comments

### Deployment

**[Deployment Guide](deployment-guide.md)** - End to end deployment workflows,
zerodowntime deployments, multi environment setups, and CI/CD integration

**[Troubleshooting](troubleshooting.md)** - Troubleshooting guide for SSH,
registry, deployment, network, container, and build issues

### Architecture

**[Architecture Overview](architecture.md)** - High level overview of Jiji's
system architecture, components, deployment flow, network topology, and security
model

### Networking

**[Network Reference](network-reference.md)** - Quick reference guide for Jiji's
private networking features including WireGuard mesh, automatic service
discovery via DNS, and daemon level DNS configuration

### Logging

**[Logs Reference](logs-reference.md)** - Guide for viewing and filtering logs
from services and kamal-proxy, including real time following, grep filtering,
and common debugging workflows

### Registry

**[Registry Reference](registry-reference.md)** - Guide for automatic namespace
detection and authentication with container registries (GHCR, Docker Hub, local
registries)

### Development

**[Testing Guide](testing.md)** - Instructions for testing Jiji deployments
including proxy testing, SSH connections, and troubleshooting
**[Version Script](version.md)** - Documentation for the version management
utility and `jiji version` command

## Quick Links

### For Users

- [Main README](../README.md) - Installation, usage, and getting started guide
- [Deployment Guide](deployment-guide.md) - Complete deployment workflows
- [Configuration Reference](configuration-reference.md) - All configuration
  options
- [Troubleshooting](troubleshooting.md) - Common issues and solutions

### For Contributors

- [Architecture Overview](architecture.md) - System architecture and design
- [Testing Guide](testing.md) - How to test your changes
- [Network Reference](network-reference.md) - Understanding networking layer
- [Logs Reference](logs-reference.md) - Debugging with logs

## Getting Help

[Issues](https://github.com/acidtib/jiji/issues) - Report bugs or request
features [Discussions](https://github.com/acidtib/jiji/discussions) - Ask
questions and share ideas
