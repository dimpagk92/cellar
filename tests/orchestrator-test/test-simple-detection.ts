/**
 * Quick test: does isSimpleGoal() correctly skip decomposition for MiniWoB goals?
 */

// Inline the detection logic for testing (same as orchestrator.ts)
const SIMPLE_VERB_PATTERNS = [
  /^click\s/i, /^type\s/i, /^press\s/i, /^scroll\s/i, /^wait\s/i,
  /^extract\s/i, /^open\s/i, /^close\s/i, /^select\s/i, /^fill\s/i,
  /^enter\s/i, /^submit\s/i, /^check\s/i, /^uncheck\s/i, /^toggle\s/i,
  /^focus\s/i, /^find\s/i, /^search\s/i, /^like\s/i, /^reply\s/i,
  /^navigate\s/i, /^choose\s/i, /^pick\s/i, /^drag\s/i, /^drop\s/i,
];

const MULTI_SCREEN_SIGNALS = [
  /\bthen open\b/i,
  /\bswitch to\b.*\bapp\b/i,
  /\bopen\s+(slack|chrome|finder|mail|excel|safari|terminal)\b/i,
  /\bnavigate to\b.*\bthen\b/i,
  /\bcopy\b.*\bpaste\b/i,
  /\bfrom\b.*\bto\b.*\b(app|window|tab)\b/i,
];

function isSimpleGoal(goal: string, elementCount?: number): boolean {
  const trimmed = goal.trim();
  if (MULTI_SCREEN_SIGNALS.some((p) => p.test(trimmed))) return false;
  if (SIMPLE_VERB_PATTERNS.some((p) => p.test(trimmed))) return true;
  if (elementCount !== undefined && elementCount <= 20) return true;
  if (trimmed.length < 80 && !/\b(then|after that|next|finally|afterwards)\b/i.test(trimmed)) return true;
  return false;
}

// MiniWoB goals (should ALL be simple → skip decomposition)
const SIMPLE_GOALS = [
  "Click the button that says 'Click Me!'",
  "Click on the 'Submit' button.",
  "Click button ONE, then click button TWO.",
  "Click on the link that says 'Privacy Policy'.",
  "Click the button in the dialog box to close it.",
  "Click the 'x' to close the dialog, then click 'Submit'.",
  "Type 'hello world' into the text field and press Submit.",
  "Enter the password 'abc123' and click Login.",
  "Enter username 'testuser' and password 'pass123', then click Login.",
  "Wait for the text field to appear, then type 'dynamic text'.",
  "Navigate to 'Section 2 > Item 3' in the tree view.",
  "Click on the 'Tab 2' tab.",
  "Switch to Tab 2, then click the button inside it.",
  "Click on the input field to focus it, then type 'hello'.",
  "Focus the second text field and type 'world'.",
  "Select the checkboxes for 'Option A' and 'Option C', then click Submit.",
  "Select the date December 25, 2024 from the date picker.",
  "Type 'restaurants near me' in the search box and click Search.",
  "Like the post by 'Alice' and reply with 'Great post!'.",
  "Open the email from 'Bob' and click Reply.",
  // Additional real MiniWoB goals
  "Fill in the name field with 'John Doe' and the email field with 'john@example.com', then click Submit",
];

// Complex goals (should NOT skip decomposition)
const COMPLEX_GOALS = [
  "Find the quarterly earnings data in the spreadsheet, copy the revenue number, open Slack, and send it to the #finance channel",
  "Open Chrome, search for 'Anthropic Claude pricing', find the per-token cost, and paste it into TextEdit",
  "Copy the address from the email then open Google Maps and search for it",
  "Open Finder and find the report.pdf, then open Mail and attach it to a new email",
];

console.log("=== isSimpleGoal() Detection Test ===\n");

let passed = 0;
let failed = 0;

console.log("SIMPLE GOALS (should skip decomposition):");
for (const goal of SIMPLE_GOALS) {
  // MiniWoB pages have ~3-15 elements
  const result = isSimpleGoal(goal, 10);
  const status = result ? "OK" : "MISS";
  if (result) passed++; else failed++;
  console.log(`  ${status}  ${goal.slice(0, 70)}`);
}

console.log("\nCOMPLEX GOALS (should decompose):");
for (const goal of COMPLEX_GOALS) {
  const result = isSimpleGoal(goal, 50); // complex pages have more elements
  const status = !result ? "OK" : "MISS";
  if (!result) passed++; else failed++;
  console.log(`  ${status}  ${goal.slice(0, 70)}`);
}

console.log(`\n${passed}/${passed + failed} correct (${((passed / (passed + failed)) * 100).toFixed(0)}%)`);
if (failed > 0) console.log(`${failed} incorrect detections`);
