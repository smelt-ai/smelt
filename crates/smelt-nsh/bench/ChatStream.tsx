import { mount } from "nsh";
import { ChatStream } from "./chat_stream";

mount(() => <ChatStream intervalMs={16} />);
