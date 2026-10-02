import { Check, ChevronDown, PencilLine, ShieldAlert, ShieldCheck, X } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import type { PlanStep, QuestionPrompt } from "./runtime";

interface ApprovalCardProps {
  prompts: QuestionPrompt[];
  plan: PlanStep[];
  onRespond: (response: string) => void;
  onEditPlan: () => void;
}

const normalized = (value: string) => value.trim().toLowerCase();

function optionKind(label: string): "approve" | "approveClass" | "reject" | "edit" | "other" {
  const value = normalized(label);
  if (value.includes("approve class") || value.includes("always allow")) return "approveClass";
  if (value.includes("approve") || value.includes("allow once")) return "approve";
  if (value.includes("reject") || value.includes("deny")) return "reject";
  if (value.includes("feedback") || value.includes("modify") || value.includes("edit plan")) return "edit";
  return "other";
}

function isApprovalPrompt(prompt: QuestionPrompt): boolean {
  const header = normalized(prompt.header);
  return header.includes("permission") || header.includes("approval") || prompt.options.some((option) => optionKind(option.label) !== "other");
}

function approvalFacts(question: string): Array<{ label: string; value: string }> {
  const labels = new Set(["Action", "Why", "Scope", "Risk", "Command", "Access"]);
  return question
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const separator = line.indexOf(":");
      if (separator < 1) return undefined;
      const label = line.slice(0, separator).trim();
      const value = line.slice(separator + 1).trim();
      if (!labels.has(label) || !value) return undefined;
      return { label, value };
    })
    .filter((fact): fact is { label: string; value: string } => Boolean(fact));
}

export function ApprovalCard({ prompts, plan, onRespond, onEditPlan }: ApprovalCardProps) {
  const [expanded, setExpanded] = useState(true);
  const approvalPrompts = useMemo(() => prompts.filter(isApprovalPrompt), [prompts]);
  const otherPrompts = useMemo(() => prompts.filter((prompt) => !isApprovalPrompt(prompt)), [prompts]);
  const completed = plan.filter((step) => step.status === "completed").length;

  useEffect(() => {
    if (!expanded || !approvalPrompts.length) return;
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (target?.matches("input, textarea, select, [contenteditable='true']")) return;
      const prompt = approvalPrompts[0];
      const shortcut = event.key.toLowerCase();
      const option = shortcut === "y"
        ? prompt.options.find((item) => optionKind(item.label) === "approve")
        : shortcut === "n"
          ? prompt.options.find((item) => optionKind(item.label) === "reject")
          : undefined;
      if (!option) return;
      event.preventDefault();
      onRespond(option.label);
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [approvalPrompts, expanded, onRespond]);

  if (!prompts.length) return null;

  return (
    <section className="approval-card" aria-live="assertive" aria-label="Medusa approval request">
      <header className="approval-header">
        <span className="approval-icon"><ShieldAlert size={18} /></span>
        <div>
          <small>Operator decision required</small>
          <strong>{approvalPrompts.length ? "Review the requested action" : otherPrompts[0]?.question}</strong>
        </div>
        <button className="approval-expand" onClick={() => setExpanded((value) => !value)} aria-expanded={expanded} aria-label={expanded ? "Collapse approval details" : "Expand approval details"}>
          <ChevronDown size={16} className={expanded ? "expanded" : ""} />
        </button>
      </header>

      {expanded && (
        <div className="approval-details">
          {!!plan.length && (
            <details className="approval-plan-details">
              <summary>Execution plan <small>{completed}/{plan.length} complete</small></summary>
              <div className="approval-plan">
                <div className="approval-plan-heading"><span>Execution plan</span><small>{completed}/{plan.length} complete</small></div>
                {plan.map((step) => <div key={step.title} className={`approval-plan-step ${step.status}`}><span>{step.title}</span><small>{step.status.replace("inProgress", "in progress")}</small></div>)}
              </div>
            </details>
          )}

          {approvalPrompts.map((prompt) => {
            const facts = approvalFacts(prompt.question);
            return (
            <div className="approval-prompt" key={`${prompt.header}-${prompt.question}`}>
              {facts.length ? (
                <dl className="approval-facts">
                  {facts.map((fact) => <div key={fact.label}><dt>{fact.label}</dt><dd>{fact.value}</dd></div>)}
                </dl>
              ) : <p>{prompt.question}</p>}
              <div className="approval-actions">
                {prompt.options.map((option) => {
                  const kind = optionKind(option.label);
                  if (kind === "edit") {
                    return <button key={option.label} className="approval-action edit" onClick={onEditPlan}><PencilLine size={15} /><span>{option.label}</span><small>{option.description}</small></button>;
                  }
                  const Icon = kind === "reject" ? X : kind === "approveClass" ? ShieldCheck : Check;
                  return <button key={option.label} className={`approval-action ${kind}`} onClick={() => onRespond(option.label)} autoFocus={kind === "approve"} aria-keyshortcuts={kind === "approve" ? "Y" : kind === "reject" ? "N" : undefined}><Icon size={15} /><span>{option.label}</span><small>{option.description}</small></button>;
                })}
              </div>
            </div>
            );
          })}

          {otherPrompts.map((prompt) => (
            <div className="approval-prompt" key={`${prompt.header}-${prompt.question}`}>
              <p>{prompt.question}</p>
              <div className="approval-actions">
                {prompt.options.map((option) => <button key={option.label} className="approval-action other" onClick={() => onRespond(option.label)}><span>{option.label}</span><small>{option.description}</small></button>)}
              </div>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
