import ghidra.app.script.GhidraScript;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.*;
import java.util.*;
public class Callers extends GhidraScript {
  public void run() throws Exception {
    for (String name : getScriptArgs()) {
      for (Function f : currentProgram.getFunctionManager().getFunctions(true)) {
        if (!f.getName().equals(name)) continue;
        Set<String> s = new TreeSet<>();
        for (Reference r : getReferencesTo(f.getEntryPoint())) {
          Function c = getFunctionContaining(r.getFromAddress());
          if (c != null) s.add(c.getName());
        }
        println("CALLERS " + name + ": " + String.join(" ", s));
      }
    }
  }
}
