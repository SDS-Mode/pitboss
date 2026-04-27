<script lang="ts">
  import { postControlOp, ApiError } from '$lib/api';
  import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogFooter,
    DialogHeader,
    DialogTitle
  } from '$lib/components/ui/dialog';
  import { Button } from '$lib/components/ui/button';
  import { Badge } from '$lib/components/ui/badge';
  import { Input } from '$lib/components/ui/input';
  import { CheckCircle2, XCircle, AlertTriangle } from 'lucide-svelte';

  /**
   * `ApprovalRequest` envelope from the dispatcher. Treated loosely so
   * server-side schema additions don't break the modal — only the
   * fields we render are typed.
   */
  export type ApprovalRequest = {
    request_id: string;
    task_id: string;
    summary: string;
    kind?: 'action' | 'plan';
    plan?: {
      summary?: string;
      rationale?: string;
      resources?: string[];
      risks?: string[];
      rollback?: string;
    };
  };

  let {
    runId,
    request = $bindable(null)
  }: {
    runId: string;
    /**
     * The request currently being prompted on. Bound — the modal clears
     * it after the operator submits, so the parent can hand in the next
     * pending request.
     */
    request: ApprovalRequest | null;
  } = $props();

  let comment = $state('');
  let editedSummary = $state('');
  let denyReason = $state('');
  let submitting = $state(false);
  let error = $state<string | null>(null);
  const open = $derived(request !== null);

  // Pre-populate the edit field with the original summary every time a
  // new request lands.
  $effect(() => {
    if (request) {
      editedSummary = request.summary;
      comment = '';
      denyReason = '';
      error = null;
    }
  });

  async function submit(approved: boolean) {
    if (!request) return;
    submitting = true;
    error = null;
    try {
      await postControlOp(runId, {
        op: 'approve',
        request_id: request.request_id,
        approved,
        comment: comment.trim() || undefined,
        edited_summary:
          approved && editedSummary.trim() !== request.summary
            ? editedSummary.trim()
            : undefined,
        reason: !approved ? denyReason.trim() || undefined : undefined
      });
      request = null;
    } catch (e) {
      error = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
    } finally {
      submitting = false;
    }
  }
</script>

<Dialog {open} onOpenChange={(v) => { if (!v && !submitting) request = null; }}>
  <DialogContent class="max-w-xl">
    {#if request}
      <DialogHeader>
        <DialogTitle class="flex items-center gap-2">
          Approval needed
          <Badge variant="outline" class="text-xs">
            {request.kind === 'plan' ? 'plan (pre-flight)' : 'action'}
          </Badge>
        </DialogTitle>
        <DialogDescription>
          Worker <code class="text-xs">{request.task_id}</code> is paused on
          this request. Approve to continue, deny to cancel.
        </DialogDescription>
      </DialogHeader>

      <div class="space-y-3 text-sm">
        <div>
          <label for="approval-summary" class="text-muted-foreground mb-1 block text-xs">
            Summary {#if request.summary !== editedSummary}<span class="text-amber-600">· edited</span>{/if}
          </label>
          <Input id="approval-summary" bind:value={editedSummary} disabled={submitting} />
        </div>

        {#if request.plan?.rationale}
          <div>
            <p class="text-muted-foreground mb-1 text-xs">Rationale</p>
            <p class="bg-muted/40 rounded p-2 text-xs leading-relaxed">{request.plan.rationale}</p>
          </div>
        {/if}

        {#if request.plan?.resources?.length}
          <div>
            <p class="text-muted-foreground mb-1 text-xs">Resources</p>
            <ul class="list-inside list-disc text-xs">
              {#each request.plan.resources as r}<li><code>{r}</code></li>{/each}
            </ul>
          </div>
        {/if}

        {#if request.plan?.risks?.length}
          <div>
            <p class="text-muted-foreground mb-1 text-xs">Risks</p>
            <ul class="list-inside list-disc text-xs">
              {#each request.plan.risks as r}<li>{r}</li>{/each}
            </ul>
          </div>
        {/if}

        {#if request.plan?.rollback}
          <div>
            <p class="text-muted-foreground mb-1 text-xs">Rollback</p>
            <p class="bg-muted/40 rounded p-2 text-xs leading-relaxed">{request.plan.rollback}</p>
          </div>
        {/if}

        <div>
          <label for="approval-comment" class="text-muted-foreground mb-1 block text-xs">
            Comment (optional)
          </label>
          <Input
            id="approval-comment"
            bind:value={comment}
            placeholder="Additional context for the worker"
            disabled={submitting}
          />
        </div>

        <div>
          <label for="deny-reason" class="text-muted-foreground mb-1 block text-xs">
            Deny reason (used only if you deny)
          </label>
          <Input
            id="deny-reason"
            bind:value={denyReason}
            placeholder="Why this is being rejected"
            disabled={submitting}
          />
        </div>

        {#if error}
          <p class="text-destructive flex items-start gap-2 text-xs">
            <AlertTriangle class="mt-0.5 size-3 shrink-0" />
            {error}
          </p>
        {/if}
      </div>

      <DialogFooter class="gap-2 sm:gap-2">
        <Button variant="outline" disabled={submitting} onclick={() => submit(false)}>
          <XCircle class="mr-1.5 size-4" /> Deny
        </Button>
        <Button disabled={submitting} onclick={() => submit(true)}>
          <CheckCircle2 class="mr-1.5 size-4" /> Approve
        </Button>
      </DialogFooter>
    {/if}
  </DialogContent>
</Dialog>
