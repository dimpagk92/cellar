import { Command } from "commander";
import { Cel } from "@cellar/agent";

export const actionCommand = new Command("action")
  .description("Execute a quick input action")
  .addCommand(
    new Command("click")
      .description("Click at screen coordinates")
      .argument("<x>", "X coordinate")
      .argument("<y>", "Y coordinate")
      .action((x: string, y: string) => {
        const cel = ensureCel();
        cel.click(parseInt(x, 10), parseInt(y, 10));
        console.log(`Clicked at (${x}, ${y})`);
      })
  )
  .addCommand(
    new Command("right-click")
      .description("Right-click at screen coordinates")
      .argument("<x>", "X coordinate")
      .argument("<y>", "Y coordinate")
      .action((x: string, y: string) => {
        const cel = ensureCel();
        cel.rightClick(parseInt(x, 10), parseInt(y, 10));
        console.log(`Right-clicked at (${x}, ${y})`);
      })
  )
  .addCommand(
    new Command("double-click")
      .description("Double-click at screen coordinates")
      .argument("<x>", "X coordinate")
      .argument("<y>", "Y coordinate")
      .action((x: string, y: string) => {
        const cel = ensureCel();
        cel.doubleClick(parseInt(x, 10), parseInt(y, 10));
        console.log(`Double-clicked at (${x}, ${y})`);
      })
  )
  .addCommand(
    new Command("type")
      .description("Type text")
      .argument("<text>", "Text to type")
      .action((text: string) => {
        const cel = ensureCel();
        cel.typeText(text);
        console.log(`Typed "${text}"`);
      })
  )
  .addCommand(
    new Command("key")
      .description("Press a key (e.g. Enter, Tab, Escape)")
      .argument("<key>", "Key name")
      .action((key: string) => {
        const cel = ensureCel();
        cel.keyPress(key);
        console.log(`Pressed key: ${key}`);
      })
  )
  .addCommand(
    new Command("combo")
      .description("Press a key combination (e.g. Ctrl C)")
      .argument("<keys...>", "Keys to press together")
      .action((keys: string[]) => {
        const cel = ensureCel();
        cel.keyCombo(keys);
        console.log(`Pressed combo: ${keys.join("+")}`);
      })
  )
  .addCommand(
    new Command("scroll")
      .description("Scroll (positive = down/right, negative = up/left)")
      .argument("<dx>", "Horizontal scroll amount")
      .argument("<dy>", "Vertical scroll amount")
      .action((dx: string, dy: string) => {
        const cel = ensureCel();
        cel.scroll(parseInt(dx, 10), parseInt(dy, 10));
        console.log(`Scrolled (${dx}, ${dy})`);
      })
  )
  .addCommand(
    new Command("move")
      .description("Move mouse to coordinates")
      .argument("<x>", "X coordinate")
      .argument("<y>", "Y coordinate")
      .action((x: string, y: string) => {
        const cel = ensureCel();
        cel.mouseMove(parseInt(x, 10), parseInt(y, 10));
        console.log(`Moved mouse to (${x}, ${y})`);
      })
  );

function ensureCel(): Cel {
  const cel = new Cel();
  if (!cel.isNativeAvailable) {
    console.error("Error: CEL native module not available.");
    process.exit(1);
  }
  return cel;
}
