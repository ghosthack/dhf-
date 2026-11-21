import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import java.io.*;
public class Decomp extends GhidraScript {
  public void run() throws Exception {
    String[] args = getScriptArgs();
    String outDir = args[0];
    DecompInterface di = new DecompInterface(); di.openProgram(currentProgram);
    for (int i = 1; i < args.length; i++) {
      for (Function f : currentProgram.getFunctionManager().getFunctions(true)) {
        if (!f.getName().equals(args[i])) continue;
        DecompileResults r = di.decompileFunction(f, 120, monitor);
        try (PrintWriter w = new PrintWriter(new File(outDir, args[i] + ".c"))) {
          w.println(r.decompileCompleted() ? r.getDecompiledFunction().getC() : "FAILED: " + r.getErrorMessage());
        }
      }
    }
  }
}
