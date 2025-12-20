#!/bin/bash

# Jiji Network Diagnostic Script
# This script helps diagnose DNS resolution and container networking issues

echo "=================================="
echo "Jiji Network Diagnostics"
echo "=================================="
echo

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Helper functions
print_header() {
    echo -e "\n${BLUE}=== $1 ===${NC}"
}

print_success() {
    echo -e "${GREEN}✓ $1${NC}"
}

print_warning() {
    echo -e "${YELLOW}⚠ $1${NC}"
}

print_error() {
    echo -e "${RED}✗ $1${NC}"
}

# 1. Check service status
print_header "Service Status"

services=("jiji-corrosion" "jiji-dns" "jiji-dns-update.timer")
for service in "${services[@]}"; do
    if systemctl is-active --quiet "$service"; then
        print_success "$service is running"
    else
        print_error "$service is not running"
        # Show recent logs
        echo "Recent logs for $service:"
        journalctl -u "$service" --no-pager -n 5 --since "1 hour ago" | sed 's/^/  /'
    fi
done

# 2. Check Corrosion database
print_header "Corrosion Database"

if command -v /opt/jiji/corrosion/corrosion >/dev/null 2>&1; then
    print_success "Corrosion binary found"

    # Test database connectivity
    if /opt/jiji/corrosion/corrosion exec --config /opt/jiji/corrosion/config.toml "SELECT 1;" >/dev/null 2>&1; then
        print_success "Corrosion database is accessible"

        # Show containers in database
        echo "Containers registered in Corrosion:"
        /opt/jiji/corrosion/corrosion exec --config /opt/jiji/corrosion/config.toml "SELECT service, ip, healthy, id FROM containers;" 2>/dev/null | while read -r line; do
            echo "  $line"
        done

        # Show services
        echo "Services registered:"
        /opt/jiji/corrosion/corrosion exec --config /opt/jiji/corrosion/config.toml "SELECT name, project FROM services;" 2>/dev/null | while read -r line; do
            echo "  $line"
        done
    else
        print_error "Cannot access Corrosion database"
    fi
else
    print_error "Corrosion binary not found"
fi

# 3. Check DNS configuration
print_header "DNS Configuration"

if [ -f /opt/jiji/dns/Corefile ]; then
    print_success "CoreDNS Corefile found"
    echo "Corefile content:"
    cat /opt/jiji/dns/Corefile | sed 's/^/  /'
else
    print_error "CoreDNS Corefile not found"
fi

if [ -f /opt/jiji/dns/hosts ]; then
    hosts_count=$(wc -l < /opt/jiji/dns/hosts)
    print_success "CoreDNS hosts file found with $hosts_count entries"
    if [ $hosts_count -gt 0 ]; then
        echo "CoreDNS hosts entries:"
        cat /opt/jiji/dns/hosts | sed 's/^/  /'
    fi
else
    print_warning "CoreDNS hosts file not found or empty"
fi

# 4. Check system hosts file
print_header "System Hosts Configuration"

if grep -q "Jiji container hostnames" /etc/hosts; then
    print_success "Jiji entries found in /etc/hosts"
    echo "Jiji entries in /etc/hosts:"
    sed -n '/# Jiji container hostnames/,/# End Jiji container hostnames/p' /etc/hosts | sed 's/^/  /'
else
    print_warning "No Jiji entries in /etc/hosts"
fi

# 5. Check container networking
print_header "Container Networking"

# Detect container engine
ENGINE="podman"
if command -v docker >/dev/null 2>&1 && systemctl is-active --quiet docker; then
    ENGINE="docker"
fi

print_success "Using container engine: $ENGINE"

# List running containers
echo "Running containers:"
$ENGINE ps --format "table {{.Names}}\t{{.Image}}\t{{.Status}}\t{{.Ports}}" | sed 's/^/  /'

# Check jiji network
if $ENGINE network ls | grep -q "jiji"; then
    print_success "Jiji network exists"
    echo "Jiji network details:"
    $ENGINE network inspect jiji | jq -r '.[] | "  Subnet: " + .IPAM.Config[0].Subnet + ", Gateway: " + .IPAM.Config[0].Gateway' 2>/dev/null || echo "  (unable to parse network details)"
else
    print_error "Jiji network not found"
fi

# Check container IPs
echo "Container IP addresses:"
for container in $($ENGINE ps --format "{{.Names}}"); do
    ip=$($ENGINE inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$container" 2>/dev/null)
    if [ -n "$ip" ]; then
        echo "  $container: $ip"
    else
        echo "  $container: No IP found"
    fi
done

# 6. Test DNS resolution
print_header "DNS Resolution Tests"

# Test system DNS
echo "System DNS servers:"
cat /etc/resolv.conf | grep nameserver | sed 's/^/  /'

# Test various hostname formats
test_hostnames=("api" "api-api" "hono-api-api" "database" "web")

for hostname in "${test_hostnames[@]}"; do
    if nslookup "$hostname" 127.0.0.1 >/dev/null 2>&1; then
        ip=$(nslookup "$hostname" 127.0.0.1 2>/dev/null | grep "Address:" | tail -1 | awk '{print $2}')
        print_success "$hostname resolves to $ip"
    else
        print_warning "$hostname does not resolve"
    fi
done

# Test external DNS
if nslookup google.com >/dev/null 2>&1; then
    print_success "External DNS resolution works"
else
    print_error "External DNS resolution fails"
fi

# 7. Test container connectivity
print_header "Container Connectivity Tests"

# Find API container
api_container=$($ENGINE ps --filter "name=api" --format "{{.Names}}" | head -1)
if [ -n "$api_container" ]; then
    api_ip=$($ENGINE inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$api_container" 2>/dev/null)
    if [ -n "$api_ip" ]; then
        print_success "Found API container: $api_container at $api_ip"

        # Test direct IP access
        if curl -s --connect-timeout 5 "http://$api_ip:8000" >/dev/null; then
            print_success "API container is accessible via direct IP"
        else
            print_error "API container is not accessible via direct IP"
        fi

        # Test hostname access
        if curl -s --connect-timeout 5 "http://api-api:8000" >/dev/null; then
            print_success "API container is accessible via hostname api-api"
        else
            print_error "API container is not accessible via hostname api-api"
        fi

        if curl -s --connect-timeout 5 "http://hono-api-api:8000" >/dev/null; then
            print_success "API container is accessible via hostname hono-api-api"
        else
            print_error "API container is not accessible via hostname hono-api-api"
        fi
    else
        print_error "Could not get IP for API container"
    fi
else
    print_warning "No API container found"
fi

# 8. Check kamal-proxy logs
print_header "Kamal-proxy Status"

if $ENGINE ps | grep -q kamal-proxy; then
    print_success "Kamal-proxy container is running"
    echo "Recent kamal-proxy logs:"
    $ENGINE logs kamal-proxy --tail 10 | sed 's/^/  /'
else
    print_warning "Kamal-proxy container not found"
fi

# 9. Network troubleshooting commands
print_header "Quick Fix Commands"

echo "To manually update DNS hosts:"
echo "  /opt/jiji/dns/update-hosts.sh"
echo

echo "To restart DNS services:"
echo "  systemctl restart jiji-dns"
echo "  systemctl restart jiji-dns-update.timer"
echo

echo "To manually add container hostname (replace with actual values):"
echo "  echo '10.89.0.X api-api' >> /etc/hosts"
echo "  echo '10.89.0.X hono-api-api' >> /etc/hosts"
echo

echo "To check container network connectivity:"
echo "  $ENGINE exec <container_name> ping api-api"
echo "  $ENGINE exec <container_name> nslookup api-api"
echo

echo "To trigger immediate Jiji DNS update:"
echo "  jiji network dns-update --diagnose"

print_header "Diagnostic Complete"
echo "If issues persist, check the logs and try the suggested fix commands above."
