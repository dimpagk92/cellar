//! Dump the accessibility tree of the focused application.
//!
//! Usage: cargo run -p cel-accessibility --example dump_tree
//!
//! Requires Accessibility permission in System Settings.
//! If denied, prints instructions.

fn main() {
    let tree = cel_accessibility::create_tree();

    match tree.get_tree() {
        Ok(root) => {
            println!("App root: {:?} {:?}", root.role, root.label);
            println!("Children: {}", root.children.len());
            println!("---");
            print_tree(&root, 0);
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            eprintln!();
            eprintln!("If you see 'Accessibility permission not granted':");
            eprintln!("  1. Open System Settings > Privacy & Security > Accessibility");
            eprintln!("  2. Add Terminal.app (or your terminal emulator)");
            eprintln!("  3. Re-run this command");
        }
    }
}

fn print_tree(elem: &cel_accessibility::AccessibilityElement, depth: usize) {
    let indent = "  ".repeat(depth);
    let label = elem.label.as_deref().unwrap_or("(none)");
    let bounds = elem
        .bounds
        .as_ref()
        .map(|b| format!("({},{} {}x{})", b.x, b.y, b.width, b.height))
        .unwrap_or_default();
    let state_flags = format!(
        "{}{}{}",
        if elem.state.focused { "F" } else { "" },
        if elem.state.enabled { "E" } else { "" },
        if elem.state.visible { "V" } else { "" },
    );
    let actions = if elem.actions.is_empty() {
        String::new()
    } else {
        format!(" [{}]", elem.actions.join(","))
    };

    println!(
        "{}{:?} \"{}\" {} {}{} (conf: n/a)",
        indent, elem.role, label, bounds, state_flags, actions
    );

    for child in &elem.children {
        print_tree(child, depth + 1);
    }
}
