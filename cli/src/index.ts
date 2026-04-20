#!/usr/bin/env node

import { Command } from "commander";
import { workflowCommand } from "./commands/workflow.js";
import { runCommand } from "./commands/run.js";
import { statusCommand } from "./commands/status.js";
import { captureCommand } from "./commands/capture.js";
import { contextCommand } from "./commands/context.js";
import { historyCommand } from "./commands/history.js";
import { memoryCommand } from "./commands/memory.js";
import { actionCommand } from "./commands/action.js";
import { mcpCommand } from "./commands/mcp.js";
import { setupCommand } from "./commands/setup.js";
import { initCommand } from "./commands/init.js";
import { runGoalCommand } from "./commands/run-goal.js";
import { browserCommand } from "./commands/browser.js";

const program = new Command();

program
  .name("dilipod")
  .description("cellar CLI — desktop agent runtime powered by CEL")
  .version("0.1.0");

program.addCommand(workflowCommand);
program.addCommand(runCommand);
program.addCommand(statusCommand);
program.addCommand(captureCommand);
program.addCommand(contextCommand);
program.addCommand(historyCommand);
program.addCommand(memoryCommand);
program.addCommand(actionCommand);
program.addCommand(mcpCommand);
program.addCommand(browserCommand);
program.addCommand(setupCommand);
program.addCommand(initCommand);
program.addCommand(runGoalCommand);

program.parse();
