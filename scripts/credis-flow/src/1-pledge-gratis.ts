import { submitPledgeNote } from "./submit-pledgenote.js";

submitPledgeNote("createPledgeNote").catch(error => { console.error(error); process.exitCode = 1; });
