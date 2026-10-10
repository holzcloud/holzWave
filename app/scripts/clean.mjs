// Empty dist before every build, so a file removed from src/ also disappears from the bundle.
import { rmSync } from "node:fs";
rmSync("dist", { recursive: true, force: true });
