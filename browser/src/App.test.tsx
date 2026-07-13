import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { App } from "./App";

describe("React application shell", () => {
  it("renders the recovery workflow with balanced effort selected", () => {
    const html = renderToStaticMarkup(<App />);
    expect(html).toContain("Bring a blurred");
    expect(html).toContain("LOAD SYNTHETIC DEMO SAMPLE");
    expect(html).toMatch(/<input[^>]*checked=""[^>]*value="balanced"/);
    expect(html).toContain("RUN RECOVERY");
  });
});
