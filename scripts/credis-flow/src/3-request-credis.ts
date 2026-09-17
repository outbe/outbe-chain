import { submitPledgeNote } from "./submit-pledgenote.js";

submitPledgeNote("issueCredis").catch(error => { console.error(error); process.exitCode = 1; });
