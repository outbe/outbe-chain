import { submitPledgeNote } from "./submit-pledgenote.js";

submitPledgeNote("cancelPledgeNote").catch(error => { console.error(error); process.exitCode = 1; });
