/**
 * IPv6 management address derivation for Corrosion gossip protocol
 *
 * Derives deterministic IPv6 addresses from WireGuard public keys
 * using the fdcc::/16 ULA (Unique Local Address) prefix.
 */

import { crypto } from "@std/crypto";

/**
 * Derive an IPv6 management address from a WireGuard public key
 *
 * Uses the first 14 bytes of the public key to create a deterministic
 * IPv6 address in the fdcc::/16 range.
 *
 * @param publicKey - WireGuard public key (base64 encoded)
 * @returns IPv6 address (e.g., "fdcc:a1b2:c3d4:e5f6:a7b8:c9d0:e1f2:a3b4")
 */
export async function deriveManagementIp(
  publicKey: string,
): Promise<string> {
  // Decode the base64 public key
  const keyBytes = decodeBase64(publicKey);

  // Hash the key to get consistent, well-distributed bytes
  const hashBuffer = await crypto.subtle.digest(
    "SHA-256",
    new Uint8Array(keyBytes),
  );
  const hashBytes = new Uint8Array(hashBuffer);

  // Take first 14 bytes for the address (after fdcc prefix)
  // fdcc:XXXX:XXXX:XXXX:XXXX:XXXX:XXXX:XXXX
  //     ^--- 14 bytes (7 groups of 2 bytes each) ---^

  const parts: string[] = ["fdcc"];

  for (let i = 0; i < 7; i++) {
    const offset = i * 2;
    const part = ((hashBytes[offset] << 8) | hashBytes[offset + 1])
      .toString(16)
      .padStart(4, "0");
    parts.push(part);
  }

  return parts.join(":");
}

/**
 * Decode base64 string to Uint8Array
 * Handles both standard and URL-safe base64
 */
function decodeBase64(input: string): Uint8Array {
  // Normalize to standard base64
  const normalized = input.replace(/-/g, "+").replace(/_/g, "/");

  // Add padding if needed
  const padded = normalized + "=".repeat((4 - (normalized.length % 4)) % 4);

  // Decode using browser-compatible method
  const binary = atob(padded);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }

  return bytes;
}

/**
 * Validate an IPv6 address format
 *
 * @param ip - IPv6 address to validate
 * @returns True if valid IPv6 format
 */
export function isValidIpv6(ip: string): boolean {
  // Simple regex for IPv6 validation
  const ipv6Regex =
    /^(([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,7}:|([0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,5}(:[0-9a-fA-F]{1,4}){1,2}|([0-9a-fA-F]{1,4}:){1,4}(:[0-9a-fA-F]{1,4}){1,3}|([0-9a-fA-F]{1,4}:){1,3}(:[0-9a-fA-F]{1,4}){1,4}|([0-9a-fA-F]{1,4}:){1,2}(:[0-9a-fA-F]{1,4}){1,5}|[0-9a-fA-F]{1,4}:((:[0-9a-fA-F]{1,4}){1,6})|:((:[0-9a-fA-F]{1,4}){1,7}|:)|fe80:(:[0-9a-fA-F]{0,4}){0,4}%[0-9a-zA-Z]{1,}|::(ffff(:0{1,4}){0,1}:){0,1}((25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])\.){3}(25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])|([0-9a-fA-F]{1,4}:){1,4}:((25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])\.){3}(25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9]))$/;

  return ipv6Regex.test(ip);
}

/**
 * Compress an IPv6 address by removing leading zeros and
 * replacing the longest sequence of zeros with ::
 *
 * @param ip - IPv6 address to compress
 * @returns Compressed IPv6 address
 */
export function compressIpv6(ip: string): string {
  const parts = ip.split(":");

  // Remove leading zeros from each part
  const normalized = parts.map((part) => {
    return part.replace(/^0+/, "") || "0";
  });

  // Find the longest sequence of consecutive "0" parts
  let longestZeroStart = -1;
  let longestZeroLength = 0;
  let currentZeroStart = -1;
  let currentZeroLength = 0;

  for (let i = 0; i < normalized.length; i++) {
    if (normalized[i] === "0") {
      if (currentZeroStart === -1) {
        currentZeroStart = i;
        currentZeroLength = 1;
      } else {
        currentZeroLength++;
      }

      if (currentZeroLength > longestZeroLength) {
        longestZeroStart = currentZeroStart;
        longestZeroLength = currentZeroLength;
      }
    } else {
      currentZeroStart = -1;
      currentZeroLength = 0;
    }
  }

  // Replace the longest sequence with ::
  if (longestZeroLength > 1) {
    const before = normalized.slice(0, longestZeroStart);
    const after = normalized.slice(longestZeroStart + longestZeroLength);

    if (before.length === 0 && after.length === 0) {
      return "::";
    } else if (before.length === 0) {
      return "::" + after.join(":");
    } else if (after.length === 0) {
      return before.join(":") + "::";
    } else {
      return before.join(":") + "::" + after.join(":");
    }
  }

  return normalized.join(":");
}

/**
 * Expand a compressed IPv6 address to full form
 *
 * @param ip - Compressed IPv6 address
 * @returns Expanded IPv6 address
 */
export function expandIpv6(ip: string): string {
  // Handle :: expansion
  if (ip.includes("::")) {
    const parts = ip.split("::");
    const left = parts[0] ? parts[0].split(":") : [];
    const right = parts[1] ? parts[1].split(":") : [];

    const zerosNeeded = 8 - left.length - right.length;
    const zeros = Array(zerosNeeded).fill("0000");

    const expanded = [...left, ...zeros, ...right];
    return expanded.map((part) => part.padStart(4, "0")).join(":");
  }

  // Just pad each part to 4 digits
  return ip.split(":").map((part) => part.padStart(4, "0")).join(":");
}

/**
 * Generate a stable server ID from hostname
 * Used for consistent IPv6 derivation when public key is not yet available
 *
 * @param hostname - Server hostname
 * @returns Deterministic server ID
 */
export async function deriveServerIdFromHostname(
  hostname: string,
): Promise<string> {
  const encoder = new TextEncoder();
  const data = encoder.encode(hostname);
  const hashBuffer = await crypto.subtle.digest("SHA-256", data);
  const hashArray = new Uint8Array(hashBuffer);

  // Convert to hex string
  return Array.from(hashArray)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("")
    .substring(0, 16); // Take first 16 characters
}
