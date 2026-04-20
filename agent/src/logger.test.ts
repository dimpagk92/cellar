import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { log, createLogger, setLogLevel, getLogLevel } from "./logger.js";

describe("logger", () => {
  let errorSpy: ReturnType<typeof vi.spyOn>;
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    setLogLevel("debug"); // Enable all levels for tests
  });

  afterEach(() => {
    errorSpy.mockRestore();
    warnSpy.mockRestore();
    setLogLevel("info"); // Reset
  });

  it("should log info messages", () => {
    log.info("test message", { key: "value" });
    expect(errorSpy).toHaveBeenCalledTimes(1);
    const output = JSON.parse(errorSpy.mock.calls[0][0] as string);
    expect(output.level).toBe("info");
    expect(output.msg).toBe("test message");
    expect(output.key).toBe("value");
    expect(output.ts).toBeDefined();
  });

  it("should log error to stderr", () => {
    log.error("error message");
    expect(errorSpy).toHaveBeenCalledTimes(1);
    const output = JSON.parse(errorSpy.mock.calls[0][0] as string);
    expect(output.level).toBe("error");
  });

  it("should log warn to stderr", () => {
    log.warn("warning");
    expect(warnSpy).toHaveBeenCalledTimes(1);
    const output = JSON.parse(warnSpy.mock.calls[0][0] as string);
    expect(output.level).toBe("warn");
  });

  it("should respect log level filtering", () => {
    setLogLevel("error");
    log.debug("should not appear");
    log.info("should not appear");
    log.warn("should not appear");
    log.error("should appear");
    expect(errorSpy).toHaveBeenCalledTimes(1);
    expect(warnSpy).not.toHaveBeenCalled();
  });

  it("should create scoped logger with module name", () => {
    const scoped = createLogger("test-module");
    scoped.info("scoped message");
    const output = JSON.parse(errorSpy.mock.calls[0][0] as string);
    expect(output.module).toBe("test-module");
  });

  it("should get and set log level", () => {
    setLogLevel("warn");
    expect(getLogLevel()).toBe("warn");
    setLogLevel("info");
    expect(getLogLevel()).toBe("info");
  });
});
