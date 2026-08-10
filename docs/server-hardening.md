# Harden an Ubuntu Server for Jiji

This guide prepares an Ubuntu server that uses the `root` account and SSH key authentication.

Keep your current SSH session open until a second SSH connection succeeds.

## 1. Update the server

Run these commands as `root`:

```bash
apt update
apt full-upgrade -y
apt install -y openssh-server ufw unattended-upgrades ca-certificates curl
systemctl enable --now ssh
systemctl enable --now unattended-upgrades
```

If the server requires a restart, run this command:

```bash
if [ -f /var/run/reboot-required ]; then
  reboot
fi
```

Reconnect after the server restarts.

## 2. Secure the root SSH key

Create the SSH directory and set its ownership:

```bash
install -d -m 700 -o root -g root /root/.ssh
chmod 600 /root/.ssh/authorized_keys
chown root:root /root/.ssh/authorized_keys
```

From your computer, make sure that key authentication works:

```bash
ssh -o PreferredAuthentications=publickey root@SERVER_IP
```

Keep the original SSH session open.

## 3. Harden OpenSSH

Create an OpenSSH configuration file:

```bash
install -d -m 755 /etc/ssh/sshd_config.d

tee /etc/ssh/sshd_config.d/00-jiji-hardening.conf >/dev/null <<'EOF'
PermitRootLogin prohibit-password
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitEmptyPasswords no

AllowUsers root
MaxAuthTries 3
LoginGraceTime 30
MaxSessions 10

X11Forwarding no
AllowAgentForwarding no

# Jiji requires SSH command execution. Local registry builds can also use
# a loopback-only reverse SSH tunnel.
AllowTcpForwarding yes
GatewayPorts no
PermitTTY yes
EOF
```

Make sure that the OpenSSH configuration is valid:

```bash
sshd -t
```

Show the effective values:

```bash
sshd -T | grep -E \
  'permitrootlogin|pubkeyauthentication|passwordauthentication|kbdinteractiveauthentication|allowusers|allowtcpforwarding|gatewayports'
```

The important values are:

```text
permitrootlogin without-password
pubkeyauthentication yes
passwordauthentication no
kbdinteractiveauthentication no
allowusers root
allowtcpforwarding yes
gatewayports no
```

Reload OpenSSH without closing the current session:

```bash
systemctl reload ssh
```

Open another terminal and connect again:

```bash
ssh root@SERVER_IP
```

Close the original session only after the new connection succeeds.

## 4. Configure UFW

Set the default firewall policies:

```bash
ufw default deny incoming
ufw default allow outgoing
```

Allow key-authenticated SSH from all addresses:

```bash
ufw allow 22/tcp comment 'SSH required by Jiji'
```

### Public web traffic

If the server uses jiji-proxy, allow HTTP and HTTPS:

```bash
ufw allow 80/tcp comment 'HTTP and ACME'
ufw allow 443/tcp comment 'HTTPS'
```

If the project has multiple servers, allow its WireGuard UDP port between those server addresses. `jiji server setup` prints the required port.

Show the pending rules:

```bash
ufw show added
```

Enable UFW and show its status:

```bash
ufw enable
ufw status verbose
```

From another terminal, make sure that SSH still works:

```bash
ssh root@SERVER_IP
```

## 5. Check the server

Run these commands:

```bash
sshd -t
systemctl is-active ssh
systemctl is-active unattended-upgrades
ufw status verbose
ss -lntup
```

The two systemd commands must report `active`. UFW must show the SSH `ALLOW` rule.

Then run Jiji from your project directory:

```bash
jiji server setup -e production --yes
```

Ubuntu documents OpenSSH configuration snippets in its [OpenSSH server guide](https://documentation.ubuntu.com/server/how-to/security/openssh-server/).
