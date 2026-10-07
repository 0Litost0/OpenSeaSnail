// Registered cases with executable business implementations live in dedicated
// spec files. catalog.spec.ts generates placeholders only for entries absent
// here; a listed ID without a matching spec breaks catalog/discovery
// verification instead of silently falling back to a placeholder.
export const IMPLEMENTED_CASE_IDS = new Set([
  'AUTH-001', 'AUTH-002', 'AUTH-003', 'AUTH-004',
  'TOKEN-001', 'TOKEN-002',
  'ASR-001', 'ASR-002',
  'CLEAN-001', 'CLEAN-005',
  'CLEAN-002', 'CLEAN-003', 'CLEAN-004',
  'DICT-001', 'DICT-002',
  'RECOVERY-001', 'RECOVERY-002',
  'PROVIDER-001', 'SHERPA-001',
  'SYSTEM-001', 'SYSTEM-002', 'SYSTEM-003', 'SYSTEM-004',
]);
