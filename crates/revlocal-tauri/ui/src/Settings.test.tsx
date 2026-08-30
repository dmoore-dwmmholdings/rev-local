import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, within } from '@testing-library/react';
import { Settings } from './Settings';
import type { SettingsView, TargetPanel } from './ipc';

function mapped(overrides: Partial<TargetPanel> = {}): TargetPanel {
  return {
    target: 'andare',
    server: 'andare',
    bound: [
      { capability: 'create_issue', tool: 'create_work_item', from_override: false },
      { capability: 'set_status', tool: 'transition_issue', from_override: false },
    ],
    unmapped: [
      {
        capability: 'comment',
        candidates: ['comment_on_issue', 'add_comment'],
        available: ['create_work_item', 'transition_issue'],
        explanation: '`comment` is unmapped',
      },
    ],
    server_contacted: true,
    ...overrides,
  };
}

function server(): SettingsView['servers'][number] {
  return {
    id: 'andare',
    transport: 'http',
    endpoint: 'https://andare.example/mcp',
    secrets: [{ header: 'Authorization', source: 'keychain', keychain_entry: 'andare-token' }],
    tools: ['create_work_item', 'transition_issue'],
    summary: 'andare: 2 tools, 2 capabilities mapped, 1 unmapped',
    contacted: true,
  };
}

function view(overrides: Partial<SettingsView> = {}): SettingsView {
  return {
    doctor: {
      prerequisites: [{ name: 'git', health: 'ok', detail: 'git 2.44' }],
      engines: [
        {
          name: 'engine:claude-code',
          health: 'fail',
          detail: 'not on PATH',
          remediation: 'npm i -g @anthropic-ai/claude-code',
        },
      ],
      targets: [],
      platform: [],
    },
    servers: [server()],
    targets: [mapped()],
    limits: {
      daily_tokens_per_repo: 2000000,
      daily_runs_per_repo: 200,
      daily_cost_usd_per_repo: 0,
      on_exhausted: 'pause',
      transcript_retention_days: 30,
    },
    config_path: '/home/me/.config/rev-local/config.toml',
    overrides_path: '/home/me/.config/rev-local/target-overrides.json',
    target_errors: [],
    ...overrides,
  };
}

const noop = vi.fn();

function mount(v: SettingsView | null, props: Partial<Parameters<typeof Settings>[0]> = {}) {
  return render(
    <Settings
      view={v}
      busy={false}
      onRunDoctor={noop}
      onMap={noop}
      onUnmap={noop}
      onRunOnboarding={noop}
      {...props}
    />,
  );
}

describe('settings', () => {
  it('leads with the number of unmapped capabilities', () => {
    // §15's criterion is that an unmapped capability is *visible*. A count
    // somebody has to assemble by reading four panels is not.
    mount(view());

    expect(screen.getByRole('status').textContent).toMatch(/1 capability is unmapped/i);
  });

  it('offers a fix affordance listing what the server actually has', () => {
    // §11.2: reported, never guessed — and the report has to be enough to act on
    // without asking the server again.
    const onMap = vi.fn();
    mount(view(), { onMap });

    const select = screen.getByLabelText(/tool for comment/i);
    expect([...select.querySelectorAll('option')].map((o) => o.textContent)).toEqual([
      'choose a tool…',
      'create_work_item',
      'transition_issue',
    ]);

    // Nothing chosen yet: mapping to nothing is not a fix.
    expect((screen.getByRole('button', { name: /map it/i }) as HTMLButtonElement).disabled).toBe(
      true,
    );

    fireEvent.change(select, { target: { value: 'transition_issue' } });
    fireEvent.click(screen.getByRole('button', { name: /map it/i }));

    expect(onMap).toHaveBeenCalledWith('andare', 'comment', 'transition_issue');
  });

  it('never renders a secret, only that one is configured', () => {
    // The value cannot reach this component — the IPC type has no field for it.
    // This asserts the screen does not invent one either.
    mount(view());

    expect(screen.getByText(/andare-token/)).toBeTruthy();
    expect(screen.queryByText(/hunter2/)).toBeNull();
  });

  it('calls out a secret written into the config file', () => {
    mount(
      view({
        servers: [
          {
            ...server(),
            secrets: [
              {
                header: 'Authorization',
                source: 'literal',
                advice: 'written in the config file; consider {{keychain:name}} instead',
              },
            ],
          },
        ],
      }),
    );

    expect(screen.getByText(/set in the config file/i)).toBeTruthy();
    expect(screen.getByText(/consider \{\{keychain:name\}\}/i)).toBeTruthy();
  });

  it('says a mapping is unknown rather than unmapped when the server never answered', () => {
    // Showing four unmapped capabilities for a server nobody has spoken to sends
    // somebody writing overrides for tools that are almost certainly there.
    mount(
      view({
        targets: [mapped({ server_contacted: false, bound: [], unmapped: [] })],
      }),
    );

    expect(screen.getByText(/mapping is unknown/i)).toBeTruthy();
    expect(screen.queryByRole('status')).toBeNull();
  });

  it('distinguishes a manual binding from a resolved one and offers to undo it', () => {
    // ADR 0015. A table that showed them alike would make an override impossible
    // to find again.
    const onUnmap = vi.fn();
    mount(
      view({
        targets: [
          mapped({
            bound: [{ capability: 'comment', tool: 'transition_issue', from_override: true }],
            unmapped: [],
          }),
        ],
      }),
      { onUnmap },
    );

    const table = screen.getByRole('table');
    expect(within(table).getByText('you')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: 'undo' }));
    expect(onUnmap).toHaveBeenCalledWith('andare', 'comment');
  });

  it('re-runs doctor from the UI', () => {
    const onRunDoctor = vi.fn();
    mount(view(), { onRunDoctor });

    fireEvent.click(screen.getByRole('button', { name: /re-run doctor/i }));

    expect(onRunDoctor).toHaveBeenCalled();
  });

  it('disables the doctor button while it is running', () => {
    // Doctor shells out to every configured engine. A second click would start a
    // second pass whose result races the first.
    mount(view(), { busy: true });

    const button = screen.getByRole('button', { name: /running doctor/i });
    expect((button as HTMLButtonElement).disabled).toBe(true);
  });

  it('shows a failing check with the command that fixes it', () => {
    // §18: an error says what to do next.
    mount(view());

    expect(screen.getByText('engine:claude-code')).toBeTruthy();
    expect(screen.getByText(/npm i -g @anthropic-ai\/claude-code/)).toBeTruthy();
  });

  it('says doctor has not run rather than showing an empty pass', () => {
    // An empty report is "nothing ran", not "everything is fine".
    mount(
      view({ doctor: { prerequisites: [], engines: [], targets: [], platform: [] } }),
    );

    expect(screen.getByText(/doctor has not run yet/i)).toBeTruthy();
  });

  it('spells out that a zero cost budget means unlimited', () => {
    // "$0" on a budget screen looks like a repository that can spend nothing.
    mount(view());

    expect(screen.getByText('unlimited')).toBeTruthy();
  });

  it('shows a target whose config table will not parse', () => {
    // A target that will not parse is one whose publishes are silently not
    // happening, and this screen is the last place somebody would think to look.
    mount(view({ target_errors: ['target `trama` is missing `mcp_server`'] }));

    expect(screen.getByRole('alert').textContent).toMatch(/missing `mcp_server`/);
  });

  it('says it is loading rather than rendering empty settings', () => {
    mount(null);

    expect(screen.getByText(/loading settings/i)).toBeTruthy();
  });

  it('offers to run onboarding again', () => {
    // RL-1205's criterion. Onboarding that can only happen once is a thing people
    // are afraid to leave halfway.
    const onRunOnboarding = vi.fn();
    mount(view(), { onRunOnboarding });

    fireEvent.click(screen.getByRole('button', { name: /run setup again/i }));

    expect(onRunOnboarding).toHaveBeenCalled();
  });
});