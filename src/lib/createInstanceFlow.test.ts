import { describe, expect, it } from 'vitest';
import { planCreateFailure } from './createInstanceFlow';

describe('planCreateFailure', () => {
  it('keeps the wizard open when no instance was created', () => {
    const plan = planCreateFailure('create', false);

    expect(plan.toastKey).toBe('create_instance.failed');
    expect(plan.closeWizard).toBe(false);
  });

  it('closes the wizard after the instance was created but game files failed', () => {
    const plan = planCreateFailure('game-files', true);

    expect(plan.toastKey).toBe('create_instance.created_but_incomplete');
    expect(plan.closeWizard).toBe(true);
  });

  it('closes the wizard after the loader install failed', () => {
    const plan = planCreateFailure('loader', true);

    expect(plan.toastKey).toBe('create_instance.created_but_incomplete');
    expect(plan.closeWizard).toBe(true);
  });

  it('closes when the create command failed after writing the directory', () => {
    // The directory did not exist before the attempt and exists now, so the
    // instance is real even though the command reported an error.
    const plan = planCreateFailure('create', true);

    expect(plan.toastKey).toBe('create_instance.created_but_incomplete');
    expect(plan.closeWizard).toBe(true);
  });

  it('keeps the wizard open when the name already belonged to another instance', () => {
    // A duplicate name is a genuine create error: "created but incomplete"
    // would be false, and the user must pick a different name.
    const plan = planCreateFailure('create', false);

    expect(plan.toastKey).toBe('create_instance.failed');
    expect(plan.toastKey).not.toBe('create_instance.created');
    expect(plan.toastKey).not.toBe('create_instance.created_but_incomplete');
    expect(plan.closeWizard).toBe(false);
  });
});
