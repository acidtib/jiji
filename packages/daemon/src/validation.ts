/**
 * Re-export shared validation utilities.
 *
 * All validators live in @jiji/shared — this file re-exports them
 * so existing daemon imports continue to work without changes.
 */

export {
  escapeSql,
  isValidCIDR,
  isValidContainerId,
  isValidEndpoint,
  isValidIPv4,
  isValidIPv6,
  isValidServerId,
  isValidWireGuardKey,
  sql,
} from "@jiji/shared";
