/**
 * Audit and logging type definitions
 */

/**
 * Audit entry stored in remote audit logs
 */
export interface AuditEntry {
  timestamp: string;
  action: string;
  details?: Record<string, unknown>;
  user?: string;
  host?: string;
  status: "started" | "success" | "failed" | "warning";
  message?: string;
}

/**
 * Result of remote audit operation
 */
export interface RemoteAuditResult {
  host: string;
  success: boolean;
  error?: string;
}

/**
 * Lock information for deployment coordination
 */
export interface LockInfo {
  locked: boolean;
  message?: string;
  acquiredAt?: string;
  acquiredBy?: string;
  host?: string;
  pid?: number;
  version?: string;
}

/**
 * Lock manager interface for managing deployment locks
 */
export interface LockManager {
  acquire(message: string): Promise<boolean>;
  release(): Promise<boolean>;
  status(): Promise<LockInfo>;
  isLocked(): Promise<boolean>;
}
