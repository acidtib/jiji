# Jiji Architecture

High level overview of Jiji's system architecture, components, and design
patterns.

## Table of Contents

- [System Overview](#system-overview)
- [Component Architecture](#component-architecture)
- [Deployment Architecture](#deployment-architecture)
- [Network Architecture](#network-architecture)
- [Configuration System](#configuration-system)
- [SSH Management](#ssh-management)
- [Service Layer](#service-layer)
- [Data Flow](#data-flow)
- [Security Model](#security-model)

## System Overview

Jiji is a deployment orchestration tool that manages containerized applications
across multiple servers using a command line interface.

### Core Capabilities

- **Service Deployment**: Zero downtime container deployments with health checks
- **Private Networking**: WireGuard mesh VPN with automatic service discovery
- **Registry Management**: Local and remote container registry support
- **SSH Orchestration**: Parallel command execution across multiple servers
- **Audit Trail**: Logging of all operations

### Technology Stack

- **Runtime**: Deno 2.5+ (TypeScript)
- **CLI Framework**: Cliffy
- **SSH**: SSH2/node-ssh
- **Container Runtime**: Docker or Podman
- **Networking**: WireGuard, CoreDNS, Corrosion (CRDT database)

### Architecture Principles

1. **Zero downtime**: Keep old containers running until new ones are healthy
2. **Idempotent operations**: Commands can be run multiple times safely
3. **Fail safe**: Operations that fail don't leave system in broken state
4. **Distributed first**: Designed for multi server deployments
5. **Configuration as code**: All infrastructure defined in YAML

## Component Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Jiji CLI (Deno/TypeScript)                │
├─────────────────────────────────────────────────────────────┤
│                                                               │
│  ┌────────────────┐  ┌──────────────┐  ┌─────────────────┐  │
│  │   Commands     │  │ Config System│  │  SSH Manager    │  │
│  │  - init        │  │  - YAML      │  │  - Connection   │  │
│  │  - build       │  │  - Validation│  │  - Pooling      │  │
│  │  - deploy      │  │  - Env vars  │  │  - Proxy        │  │
│  │  - services    │  └──────────────┘  └─────────────────┘  │
│  │  - proxy       │                                          │
│  │  - server      │  ┌──────────────┐  ┌─────────────────┐  │
│  │  - registry    │  │   Services   │  │    Utilities    │  │
│  │  - network     │  │  - Deploy    │  │  - Logger       │  │
│  │  - audit       │  │  - Build     │  │  - Error        │  │
│  │  - lock        │  │  - Proxy     │  │  - Lock         │  │
│  └────────────────┘  │  - Registry  │  │  - Audit        │  │
│                      │  - Logs      │  │  - Version      │  │
│                      └──────────────┘  └─────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                              │
                         SSH Connection
                              │
┌─────────────────────────────▼─────────────────────────────┐
│                    Remote Servers                          │
├───────────────────────────────────────────────────────────┤
│                                                            │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐│
│  │  Docker/     │  │  WireGuard   │  │  kamal-proxy     ││
│  │  Podman      │  │  Mesh VPN    │  │  HTTP/HTTPS      ││
│  │  Containers  │  │  (jiji0)     │  │  Routing         ││
│  └──────────────┘  └──────────────┘  └──────────────────┘│
│                                                            │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐│
│  │  CoreDNS     │  │  Corrosion   │  │  Service         ││
│  │  DNS         │  │  Distributed │  │  Monitoring      ││
│  │  Resolution  │  │  KV Store    │  │  & Logs          ││
│  └──────────────┘  └──────────────┘  └──────────────────┘│
└───────────────────────────────────────────────────────────┘
```

## Deployment Architecture

### Zero Downtime Deployment Flow

```
1. Pre Deployment
   ┌────────────────────────────────────────────┐
   │ - Validate configuration                   │
   │ - Check SSH connectivity                   │
   │ - Verify registry authentication           │
   │ - Acquire deployment lock                  │
   └────────────────────────────────────────────┘
                      │
                      ▼
2. Proxy Installation
   ┌────────────────────────────────────────────┐
   │ - Install kamal-proxy (if services use it) │
   │ - Configure routing rules                  │
   │ - Setup health check endpoints             │
   └────────────────────────────────────────────┘
                      │
                      ▼
3. Container Deployment
   ┌────────────────────────────────────────────┐
   │ OLD CONTAINER        NEW CONTAINER         │
   │ (Still running)      (Being deployed)      │
   │                                             │
   │ ┌──────────┐         ┌──────────┐          │
   │ │ web:abc  │         │ web:def  │          │
   │ │ Healthy  │         │ Starting │          │
   │ └──────────┘         └──────────┘          │
   │      │                     │                │
   │      │                     ▼                │
   │      │           Health check runs         │
   │      │                     │                │
   │      │               ┌─────▼─────┐         │
   │      │               │ Healthy?  │         │
   │      │               └─────┬─────┘         │
   │      │                     │ Yes            │
   │      │                     ▼                │
   │      │           Proxy routes to new       │
   │      │                     │                │
   │      ▼                     ▼                │
   │ Stop & Remove      Now serving traffic     │
   └────────────────────────────────────────────┘
                      │
                      ▼
4. Post Deployment
   ┌────────────────────────────────────────────┐
   │ - Clean up old images (keep N versions)    │
   │ - Update service registry                  │
   │ - Release deployment lock                  │
   │ - Log audit trail                          │
   └────────────────────────────────────────────┘
```

### Health Check System

```
┌──────────────┐
│ kamal-proxy  │
└──────┬───────┘
       │
       │ Health check request
       │ GET /health every 10s
       │
       ▼
┌──────────────┐
│  Container   │
│  web:latest  │
│              │
│  /health →   │
│  200 OK      │
└──────────────┘
       │
       │ Status: healthy
       │
       ▼
┌──────────────┐
│ Traffic      │
│ routing      │
│ enabled      │
└──────────────┘
```

### Image Management

```
┌─────────────────────────────────────────────────────┐
│                   Registry                           │
│                                                      │
│  myproject/service:abc1234 ◄── Current deployment  │
│  myproject/service:def5678 ◄── Previous version    │
│  myproject/service:ghi9012 ◄── Old version         │
│  ...                                                 │
│  (Older versions cleaned by prune)                  │
└─────────────────────────────────────────────────────┘
                      │
                      │ Pull image
                      ▼
┌─────────────────────────────────────────────────────┐
│                 Local Server                         │
│                                                      │
│  myproject/service:abc1234 ◄── Running container   │
│  myproject/service:def5678 ◄── Cached image        │
│  (Older images removed by prune)                    │
└─────────────────────────────────────────────────────┘
```

## Network Architecture

### WireGuard Mesh Topology

```
┌────────────────────────────────────────────────────────┐
│                   WireGuard Mesh VPN                    │
│                                                         │
│  Server 0 (10.210.0.1)                                 │
│      │                                                  │
│      ├─────────┐                                        │
│      │         │                                        │
│      │         │                                        │
│  Server 1  Server 2                                    │
│ (10.210.1.1) (10.210.2.1)                              │
│      │         │                                        │
│      └────┬────┘                                        │
│           │                                             │
│       Server 3                                          │
│     (10.210.3.1)                                       │
│                                                         │
│  Each server:                                           │
│  - Gets /24 subnet (254 IPs)                           │
│  - Establishes peer connections to all other servers   │
│  - Routes traffic through WireGuard interface          │
└────────────────────────────────────────────────────────┘
```

### Container Networking

```
Server 1 (192.168.1.100)
┌─────────────────────────────────────────────┐
│                                              │
│  ┌────────────┐  ┌────────────┐            │
│  │ Web        │  │ API        │            │
│  │ 10.210.0.2 │  │ 10.210.0.3 │            │
│  └────────────┘  └────────────┘            │
│         │              │                     │
│         └──────┬───────┘                     │
│                │                             │
│         ┌──────▼──────┐                      │
│         │  docker0    │                      │
│         │  bridge     │                      │
│         └──────┬──────┘                      │
│                │                             │
│         ┌──────▼──────┐                      │
│         │  jiji0      │                      │
│         │  WireGuard  │  ◄───────────┐       │
│         │  10.210.0.1 │              │       │
│         └─────────────┘              │       │
│                                      │       │
└──────────────────────────────────────┼───────┘
                                       │
                    WireGuard Tunnel   │
                                       │
Server 2 (192.168.1.101)              │
┌──────────────────────────────────────┼───────┐
│                                      │       │
│  ┌────────────┐  ┌────────────┐     │       │
│  │ Database   │  │ Cache      │     │       │
│  │ 10.210.1.2 │  │ 10.210.1.3 │     │       │
│  └────────────┘  └────────────┘     │       │
│         │              │             │       │
│         └──────┬───────┘             │       │
│                │                     │       │
│         ┌──────▼──────┐              │       │
│         │  docker0    │              │       │
│         │  bridge     │              │       │
│         └──────┬──────┘              │       │
│                │                     │       │
│         ┌──────▼──────┐              │       │
│         │  jiji0      │  ◄───────────┘       │
│         │  WireGuard  │                      │
│         │  10.210.1.1 │                      │
│         └─────────────┘                      │
│                                              │
└─────────────────────────────────────────────┘
```

### Service Discovery Flow

```
Container Query: "api.jiji"
         │
         ▼
┌────────────────┐
│ Container DNS  │  Configured to use CoreDNS
│ /etc/resolv.conf│ nameserver 10.210.0.1
└────────┬───────┘
         │
         ▼
┌────────────────┐
│    CoreDNS     │  Listens on 10.210.0.1:53
│   10.210.0.1   │
└────────┬───────┘
         │
         │ Query Corrosion
         ▼
┌────────────────┐
│   Corrosion    │  CRDT database
│  Distributed   │  Synced across all servers
│   KV Store     │
└────────┬───────┘
         │
         │ Returns: ["10.210.0.3", "10.210.1.2", "10.210.2.5"]
         ▼
┌────────────────┐
│  DNS Response  │  All healthy API container IPs
│  Round robin   │  Client side load balancing
└────────────────┘
```

### IPv4 vs IPv6 Usage

**IPv4 (10.210.0.0/16):**

- WireGuard tunnel IPs
- Container IPs
- Service to service communication
- DNS resolution

**IPv6 (fdcc::/16):**

- Corrosion management communication only
- Derived deterministically from WireGuard public keys
- Not used for container traffic

## Configuration System

### Class Hierarchy

```
BaseConfiguration
    │
    ├── Configuration (Main)
    │   ├── ProjectConfiguration
    │   ├── BuilderConfiguration
    │   ├── RegistryConfiguration
    │   ├── SSHConfiguration
    │   ├── NetworkConfiguration
    │   ├── EnvironmentConfiguration
    │   └── Map<string, ServiceConfiguration>
    │
    ├── ServiceConfiguration
    │   ├── BuildConfiguration
    │   ├── ProxyConfiguration
    │   ├── HealthCheckConfiguration
    │   └── EnvironmentVariables
    │
    └── ProxyConfiguration
        └── HealthCheckConfiguration
```

### Configuration Loading Flow

```
1. Load YAML File
   ┌─────────────────────┐
   │ .jiji/deploy.yml    │
   │ or                  │
   │ jiji.<env>.yml      │
   └──────────┬──────────┘
              │
              ▼
2. Parse & Validate
   ┌─────────────────────┐
   │ YAML → TypeScript   │
   │ Schema validation   │
   │ Type checking       │
   └──────────┬──────────┘
              │
              ▼
3. Environment Substitution
   ┌─────────────────────┐
   │ ${VAR} → value      │
   │ Expand paths        │
   │ Load SSH config     │
   └──────────┬──────────┘
              │
              ▼
4. Create Configuration Objects
   ┌─────────────────────┐
   │ Configuration       │
   │ - Lazy loading      │
   │ - Caching           │
   │ - Validation        │
   └─────────────────────┘
```

## SSH Management

### Connection Pool Architecture

```
┌────────────────────────────────────────────┐
│              SSH Manager                    │
├────────────────────────────────────────────┤
│                                             │
│  ┌──────────────────────────────────────┐  │
│  │       Connection Pool (LRU)          │  │
│  ├──────────────────────────────────────┤  │
│  │ server1.example.com → SSH Connection │  │
│  │ server2.example.com → SSH Connection │  │
│  │ server3.example.com → SSH Connection │  │
│  │ ...                                   │  │
│  └──────────────────────────────────────┘  │
│                                             │
│  Features:                                  │
│  - Connection reuse                         │
│  - LRU eviction                            │
│  - Parallel execution                       │
│  - ProxyJump support                       │
│  - Key management                           │
│  - Timeout handling                         │
└────────────────────────────────────────────┘
```

### Command Execution Flow

```
1. Command Request
   ┌─────────────────────┐
   │ jiji server exec    │
   │ "docker ps"         │
   └──────────┬──────────┘
              │
              ▼
2. SSH Connection
   ┌─────────────────────┐
   │ Get from pool       │
   │ or create new       │
   └──────────┬──────────┘
              │
              ▼
3. Execute
   ┌─────────────────────┐
   │ Run command         │
   │ Capture output      │
   │ Handle errors       │
   └──────────┬──────────┘
              │
              ▼
4. Return Results
   ┌─────────────────────┐
   │ stdout/stderr       │
   │ exit code           │
   │ execution time      │
   └─────────────────────┘
```

## Service Layer

### Deployment Orchestrator

```
DeploymentOrchestrator
    │
    ├─► ProxyService
    │   ├── Install kamal-proxy
    │   ├── Configure routes
    │   └── Setup health checks
    │
    ├─► ContainerDeploymentService
    │   ├── Pull images
    │   ├── Create containers
    │   ├── Wait for health
    │   └── Stop old containers
    │
    ├─► ContainerRegistry
    │   ├── Register in network
    │   ├── Update DNS
    │   └── Track health
    │
    └─► ImagePruneService
        ├── List old images
        ├── Keep N versions
        └── Remove old images
```

### Service Responsibilities

**BuildService**:

- Build container images from Dockerfiles
- Support local and remote builds
- Handle build arguments and context

**ImagePushService**:

- Push images to registries
- Handle authentication
- Support multiple registry types

**ContainerDeploymentService**:

- Deploy containers with zero downtime
- Health check verification
- Graceful shutdown of old containers

**ProxyService**:

- Install and configure kamal-proxy
- Setup routing rules
- Configure SSL/TLS

**LogsService**:

- Fetch container logs
- Follow logs in real time
- Support grep filtering

**ContainerRegistry**:

- Register containers in network
- Update service discovery
- Track container health

## Data Flow

### Deployment Data Flow

```
User Command
    │
    ▼
┌─────────────────┐
│ Configuration   │ ◄── Load from YAML
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ SSH Connections │ ◄── Establish to all hosts
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Build Images    │ ◄── Local or remote
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Push to Registry│ ◄── Docker Hub, GHCR, local
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Deploy Proxy    │ ◄── kamal-proxy installation
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Deploy Container│ ◄── Pull, create, health check
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Configure Proxy │ ◄── Route traffic to new container
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Cleanup         │ ◄── Remove old containers & images
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Audit Log       │ ◄── Record operation
└─────────────────┘
```

## Security Model

### Authentication & Authorization

**SSH Authentication**:

- SSH keys (preferred)
- SSH agent
- Private key files
- ProxyJump/ProxyCommand for bastion hosts

**Registry Authentication**:

- Username/password
- Personal access tokens
- Environment variable substitution for secrets

**Server Access**:

- Requires sudo for system operations
- Container operations via Docker/Podman CLI
- Firewall configuration requires root

### Network Security

**WireGuard Encryption**:

- All inter server traffic encrypted
- Public key cryptography (Curve25519)
- Perfect forward secrecy

**Firewall Rules**:

- Only required ports opened
- WireGuard: UDP 51820
- Corrosion: TCP 8787
- HTTP/HTTPS: TCP 80/443

**Container Isolation**:

- Containers isolated in private network
- Not directly accessible from internet
- Exposed only through proxy or port mappings

### Secret Management

**Environment Variables**:

- Secrets loaded from environment at runtime
- Never stored in plain text in config files
- Variable substitution: `${SECRET_NAME}`

**SSH Keys**:

- Private keys never leave local machine
- Public keys stored on servers
- Keys can be password protected

**Registry Credentials**:

- Configured in `.jiji/deploy.yml` under `builder.registry`
- Environment variable substitution supported
- Registry authentication performed locally and on remote servers
