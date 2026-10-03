import { useCallback, useRef, useState } from "react";
import { Copy, Check } from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import { openUrl } from "@tauri-apps/plugin-opener";

function remarkBreaks() {
  return (tree: any) => {
    function walk(node: any): any {
      if (node.children) {
        const newChildren: any[] = [];
        for (const child of node.children) {
          if (
            child.type === "text" &&
            typeof child.value === "string" &&
            child.value.includes("\n")
          ) {
            const parts = child.value.split("\n");
            for (let i = 0; i < parts.length; i++) {
              if (i > 0) newChildren.push({ type: "break" });
              newChildren.push({ type: "text", value: parts[i] });
            }
          } else {
            newChildren.push(walk(child));
          }
        }
        node.children = newChildren;
      }
      return node;
    }
    walk(tree);
  };
}

export function MarkdownMessage({ content }: { content: string }) {
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm, remarkBreaks]}
      rehypePlugins={[rehypeHighlight]}
      components={{
        h1: ({ children }) => (
          <h1 className="mb-3 mt-6 text-2xl font-bold">{children}</h1>
        ),
        h2: ({ children }) => (
          <h2 className="mb-2 mt-5 text-xl font-bold">{children}</h2>
        ),
        h3: ({ children }) => (
          <h3 className="mb-2 mt-4 text-lg font-semibold">{children}</h3>
        ),
        h4: ({ children }) => (
          <h4 className="mb-1 mt-3 text-base font-semibold">{children}</h4>
        ),
        h5: ({ children }) => (
          <h5 className="mb-1 mt-3 text-sm font-medium">{children}</h5>
        ),
        h6: ({ children }) => (
          <h6 className="mb-1 mt-3 text-sm font-medium text-[var(--subtle)]">
            {children}
          </h6>
        ),
        p: ({ children }) => (
          <p className="mb-3 leading-7 last:mb-0">{children}</p>
        ),
        blockquote: ({ children }) => (
          <blockquote className="mb-3 border-l-4 border-[var(--border-strong)] pl-4 text-[var(--muted)] italic">
            {children}
          </blockquote>
        ),
        hr: () => <hr className="my-6 border-t border-[var(--border)]" />,
        img: ({ src, alt }) => (
          <img
            src={src}
            alt={alt ?? ""}
            className="mb-3 max-w-full rounded-lg"
          />
        ),
        a: ({ href, children }) => (
          <a
            href={href}
            className="font-medium text-[var(--primary)] underline underline-offset-2 decoration-[var(--border-strong)] hover:decoration-[var(--text)] transition-colors"
            onClick={(e) => {
              if (href) {
                e.preventDefault();
                void openUrl(href);
              }
            }}
          >
            {children}
          </a>
        ),
        ul: ({ children }) => (
          <ul className="mb-3 list-disc space-y-1 pl-5">{children}</ul>
        ),
        ol: ({ children }) => (
          <ol className="mb-3 list-decimal space-y-1 pl-5">{children}</ol>
        ),
        code: ({ children, className }) => (
          <code
            className={
              className ?? "rounded bg-[var(--code-inline)] px-1.5 py-0.5"
            }
          >
            {children}
          </code>
        ),
        pre: ({ children }) => <CodeBlock>{children}</CodeBlock>,
        table: ({ children }) => (
          <div className="mb-3 overflow-x-auto">
            <table className="w-full border-collapse border border-[var(--border)] text-sm">
              {children}
            </table>
          </div>
        ),
        thead: ({ children }) => (
          <thead className="bg-[var(--panel-soft)]">{children}</thead>
        ),
        th: ({ children }) => (
          <th className="border border-[var(--border)] px-3 py-2 text-left font-medium text-[var(--text)]">
            {children}
          </th>
        ),
        td: ({ children }) => (
          <td className="border border-[var(--border)] px-3 py-2 text-[var(--text)]">
            {children}
          </td>
        ),
        tr: ({ children }) => (
          <tr className="even:bg-[var(--panel-soft)]">{children}</tr>
        ),
      }}
    >
      {content}
    </ReactMarkdown>
  );
}

function CodeBlock({ children }: { children: React.ReactNode }) {
  const ref = useRef<HTMLPreElement>(null);
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(() => {
    const text = ref.current?.textContent ?? "";
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  }, []);

  return (
    <div className="group relative my-3">
      <button
        className="absolute right-2 top-2 z-10 rounded-md bg-[var(--panel-soft)] p-1.5 text-[var(--subtle)] opacity-0 transition hover:bg-[var(--hover)] hover:text-[var(--text)] group-hover:opacity-100"
        onClick={handleCopy}
        aria-label="Copy code"
      >
        {copied ? (
          <Check className="h-3.5 w-3.5" />
        ) : (
          <Copy className="h-3.5 w-3.5" />
        )}
      </button>
      <pre ref={ref} className="bg-[var(--code-block)] p-4 text-sm text-white">
        {children}
      </pre>
    </div>
  );
}
