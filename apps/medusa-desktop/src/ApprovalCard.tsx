import { Check, ChevronDown, PencilLine, ShieldAlert, ShieldCheck, X } from "lucide-react";
import { useMemo, useState } from "react";
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

export function ApprovalCard({ prompts, plan, onRespond, onEditPlan }: ApprovalCardProps) {
  const [expanded, setExpanded] = useState(true);
  const approvalPrompts = useMemo(() => prompts.filter(isApprovalPrompt), [prompts]);
  const otherPrompts = useMemo(() => prompts.filter((prompt) => !isApprovalPrompt(prompt)), [prompts]);
  const completed = plan.filter((step) => step.status === "completed").length;

  if (!prompts.length) return null;

  return (
    <section className="approval-card" aria-live="assertive" aria-label="Medusa approval request">
      <header className="approval-header">
        <span className="approval-icon"><ShieldAlert size={18} /></span>
        <div>
          <small>Operator decision required</small>
          <strong>{approvalPrompts[0]?.question ?? otherPrompts[0]?.question}</strong>
        </div>
        <button className="approval-expand" onClick={() => setExpanded((value) => !value)} aria-expanded={expanded} aria-label={expanded ? "Collapse approval details" : "Expand approval details"}>
          <ChevronDown size={16} className={expanded ? "expanded" : ""} />
        </button>
      </header>

      {expanded && (
        <div className="approval-details">
          {!!plan.length && (
            <div className="approval-plan">
              <div className="approval-plan-heading"><span>Execution plan</span><small>{completed}/{plan.length} complete</small></div>
              {plan.map((step) => <div key={step.title} className={`approval-plan-step ${step.status}`}><span>{step.title}</span><small>{step.status.replace("inProgress", "in progress")}</small></div>)}
            </div>
          )}

          {approvalPrompts.map((prompt) => (
            <div className="approval-prompt" key={`${prompt.header}-${prompt.question}`}>
              <p>{prompt.question}</p>
              <div className="approval-actions">
                {prompt.options.map((option) => {
                  const kind = optionKind(option.label);
                  if (kind === "edit") {
                    return <button key={option.label} className="approval-action edit" onClick={onEditPlan}><PencilLine size={15} /><span>{option.label}</span><small>{option.description}</small></button>;
                  }
                  const Icon = kind === "reject" ? X : kind === "approveClass" ? ShieldCheck : Check;
                  return <button key={option.label} className={`approval-action ${kind}`} onClick={() => onRespond(option.label)} autoFocus={kind === "approve"}><Icon size={15} /><span>{option.label}</span><small>{option.description}</small></button>;
                })}
              </div>
            </div>
          ))}

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
