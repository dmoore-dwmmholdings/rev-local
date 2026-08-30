import { describe, expect, it } from 'vitest';
import { reasonForApproval, reasonForFinding } from './ipc';
import type { QueuedAction } from './ipc';

describe('notification reasons', () => {
  it('identifies a finding by its fingerprint, not its title', () => {
    // §10.3's fingerprint is what makes two runs over one unfixed bug a single
    // notification. Sending the title would look identical until somebody
    // reworded a finding, and then they get told twice about the same thing.
    const reason = reasonForFinding(
      { severity: 'critical', fingerprint: 'fp-sql', title: 'SQL injection' },
      'acme',
    );

    expect(reason).toEqual({
      kind: 'finding',
      severity: 'critical',
      fingerprint: 'fp-sql',
      title: 'SQL injection',
      repo: 'acme',
    });
  });

  it('does not filter by severity before asking', () => {
    // The rule lives in the daemon, where it is tested. A front end that dropped
    // medium findings before asking would be a second copy of it, and the copy
    // that drifts is the one nobody is testing.
    const reason = reasonForFinding(
      { severity: 'info', fingerprint: 'fp-nit', title: 'trailing whitespace' },
      'acme',
    );

    expect(reason.kind).toBe('finding');
    expect((reason as { severity: string }).severity).toBe('info');
  });

  it('identifies an approval by its action id', () => {
    // Two actions that happen to target the same place are two decisions, and
    // keying on the target would hide one behind the other.
    const action: QueuedAction = {
      id: 7,
      run_id: 3,
      target: 'github',
      capability: 'post_review',
      risk: 'high',
      payload_json: '{}',
      has_finding: true,
    };

    expect(reasonForApproval(action)).toEqual({
      kind: 'approval',
      action_id: 7,
      target: 'github',
      capability: 'post_review',
    });
  });
});
