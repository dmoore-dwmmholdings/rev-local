import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { Onboarding } from './Onboarding';
import { emptyDraft, type Draft, type DoctorReport, type FirstReview, type Step } from './ipc';

const noop = vi.fn();

function doctor(): DoctorReport {
  return {
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
  };
}

function mount(
  step: Step,
  props: Partial<Parameters<typeof Onboarding>[0]> = {},
  draft: Draft = emptyDraft(),
) {
  return render(
    <Onboarding
      step={step}
      draft={draft}
      doctor={null}
      review={null}
      busy={false}
      error={null}
      onDraft={noop}
      onBack={noop}
      onNext={noop}
      onFinish={noop}
      {...props}
    />,
  );
}

describe('onboarding', () => {
  it('shows all five steps and marks where you are', () => {
    mount('pick_engine');

    const steps = screen.getByLabelText('steps');
    expect(steps.querySelectorAll('li')).toHaveLength(5);
    expect(
      [...steps.querySelectorAll('li')].find((li) => li.getAttribute('aria-current') === 'step')
        ?.textContent,
    ).toMatch(/engine/i);
  });

  it('does not block the walk on a failing check', () => {
    // §8.4 puts doctor first so somebody learns what is missing — not so they are
    // stopped. The mock engine works on any machine, and refusing to continue
    // would teach them nothing about whether rev-local works.
    mount('check', { doctor: doctor() });

    expect(screen.getByText('engine:claude-code')).toBeTruthy();
    expect(
      (screen.getByRole('button', { name: /continue/i }) as HTMLButtonElement).disabled,
    ).toBe(false);
  });

  it('will not continue without a repository, and says why', () => {
    // A control that is simply dead teaches nothing about what is wrong.
    mount('add_repo');

    expect(
      (screen.getByRole('button', { name: /continue/i }) as HTMLButtonElement).disabled,
    ).toBe(true);
    // Matched on the hint's own words: "Choose a repository" is also the step
    // title in the progress list, and a bare text query would pass on that.
    expect(screen.getByText(/directory or URL to continue/i)).toBeTruthy();
  });

  it('continues once a repository is named', () => {
    const onNext = vi.fn();
    mount('add_repo', { onNext }, { ...emptyDraft(), path: '/home/me/acme' });

    const button = screen.getByRole('button', { name: /continue/i });
    expect((button as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(button);
    expect(onNext).toHaveBeenCalled();
  });

  it('starts at dry run rather than auto', () => {
    // The criterion, where somebody would actually see it: a repository added a
    // moment ago has never been reviewed and nobody has seen a finding from it.
    mount('pick_autonomy');

    expect((screen.getByLabelText('autonomy') as HTMLSelectElement).value).toBe('dry_run');
  });

  it('warns when somebody chooses auto, without refusing it', () => {
    // §12.2's modes exist to be chosen. A safety property that cannot be switched
    // off is a bug report waiting to happen — but this one deserves a sentence.
    mount('pick_autonomy', {}, { ...emptyDraft(), autonomy: 'auto' });

    expect(screen.getByText(/without asking, before you have seen any of them/i)).toBeTruthy();
    expect(
      (screen.getByRole('button', { name: /continue/i }) as HTMLButtonElement).disabled,
    ).toBe(false);
  });

  it('defaults to an engine that spends nothing', () => {
    mount('pick_engine');

    expect((screen.getByLabelText('engine') as HTMLSelectElement).value).toBe('mock');
  });

  it('says a mock review was a rehearsal', () => {
    // §18. A rehearsal that reads like a real review is the worst possible first
    // impression: everything after it is judged against invented findings.
    const review: FirstReview = {
      run_id: 3,
      repo: 'acme',
      status: 'done',
      verdict: 'request_changes',
      findings: 2,
      engine: 'mock',
      caveat: 'This was the mock engine: it spends nothing and invents its findings.',
    };
    mount('first_review', { review });

    expect(screen.getByText(/invents its findings/i)).toBeTruthy();
    expect(screen.getByText(/2 findings/i)).toBeTruthy();
  });

  it('ends at a result, not at a configured repository', () => {
    // §15's path ends at "show the result": somebody who has added a repository
    // and seen nothing does not yet know whether any of it works.
    const review: FirstReview = {
      run_id: 3,
      repo: 'acme',
      status: 'done',
      findings: 0,
      engine: 'claude',
    };
    const onFinish = vi.fn();
    mount('first_review', { review, onFinish });

    fireEvent.click(screen.getByRole('button', { name: /done/i }));
    expect(onFinish).toHaveBeenCalled();
  });

  it('shows an error where it happened rather than in a corner', () => {
    mount('add_repo', { error: 'a repository called acme already exists' });

    expect(screen.getByRole('alert').textContent).toMatch(/already exists/);
  });

  it('cannot go back from the first step', () => {
    mount('check');

    expect((screen.getByRole('button', { name: /back/i }) as HTMLButtonElement).disabled).toBe(
      true,
    );
  });
});
