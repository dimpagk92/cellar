export interface GoalRequirement {
  label: string;
  aliases: string[];
}

export interface GoalContract {
  requirements: GoalRequirement[];
  requireHeadline: boolean;
}

const UPPERCASE_STOPWORDS = new Set([
  "USD",
  "JSON",
  "URL",
  "HTTP",
  "HTTPS",
  "CDP",
  "AX",
  "MCP",
  "CLI",
  "TS",
  "JS",
  "LLM",
  "API",
]);

export function inferGoalContract(goal: string): GoalContract {
  const requirements = dedupeRequirements([
    ...extractTickerRequirements(goal),
    ...extractUrlRequirements(goal),
  ]);
  return {
    requirements,
    requireHeadline: /\bheadline\b|\bnews\b/i.test(goal),
  };
}

export function renderGoalContract(contract: GoalContract): string {
  const lines = [];

  if (contract.requirements.length > 0) {
    lines.push("Required entities:");
    for (const requirement of contract.requirements) {
      lines.push(`- ${requirement.label}`);
    }
  }

  if (contract.requireHeadline) {
    lines.push("Required output:");
    lines.push("- one news headline");
  }

  return lines.length > 0 ? lines.join("\n") : "(none)";
}

export function evaluateDraftAnswer(
  contract: GoalContract,
  draftAnswer: string,
): { verified: boolean; missing: string[]; reason: string } {
  const missing = [];
  const haystack = draftAnswer.toLowerCase();

  for (const requirement of contract.requirements) {
    const matched = requirement.aliases.some((alias) => haystack.includes(alias.toLowerCase()));
    if (!matched) {
      missing.push(requirement.label);
    }
  }

  if (contract.requireHeadline && !looksLikeHeadlineMentioned(draftAnswer)) {
    missing.push("headline");
  }

  return {
    verified: missing.length === 0,
    missing,
    reason: missing.length === 0
      ? "Draft answer covers the required outputs"
      : `Missing required outputs: ${missing.join(", ")}`,
  };
}

function extractTickerRequirements(goal: string): GoalRequirement[] {
  const requirements: GoalRequirement[] = [];
  const matches = goal.match(/\b[A-Z]{2,10}(?:-[A-Z]{2,10})?\b/g) ?? [];

  for (const raw of matches) {
    if (UPPERCASE_STOPWORDS.has(raw)) {
      continue;
    }
    if (raw.includes("-")) {
      const [base] = raw.split("-", 1);
      requirements.push({
        label: raw,
        aliases: [raw, base],
      });
      continue;
    }
    requirements.push({
      label: raw,
      aliases: [raw],
    });
  }

  return requirements;
}

function extractUrlRequirements(goal: string): GoalRequirement[] {
  const requirements: GoalRequirement[] = [];
  const matches = goal.match(/\bhttps?:\/\/[^\s),]+/g) ?? [];

  for (const raw of matches) {
    let stripCount = 0;
    while (stripCount < raw.length && raw[raw.length - 1 - stripCount] === "/") stripCount++;
    const normalized = stripCount === 0 ? raw : raw.slice(0, raw.length - stripCount);
    requirements.push({
      label: raw,
      aliases: normalized === raw ? [raw] : [raw, normalized],
    });
  }

  return requirements;
}

function dedupeRequirements(requirements: GoalRequirement[]): GoalRequirement[] {
  const seen = new Set<string>();
  const deduped: GoalRequirement[] = [];

  for (const requirement of requirements) {
    if (seen.has(requirement.label)) {
      continue;
    }
    seen.add(requirement.label);
    deduped.push(requirement);
  }

  return deduped;
}

function looksLikeHeadlineMentioned(text: string): boolean {
  if (/\bheadline\b|\bnews\b/i.test(text)) {
    return true;
  }
  if (/"[^"\n]{12,}"/.test(text)) {
    return true;
  }
  if (/“[^”\n]{12,}”/.test(text)) {
    return true;
  }
  return false;
}
