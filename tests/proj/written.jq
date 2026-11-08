def history: {totalCount:0,nodes:[]};
def text: {author:.user,body,userContentEdits:(.userContentEdits // history)};
{data:{repository:{issue:($i[0] | text + {title,timelineItems:(.timelineItems // history)}) +
  {comments:($c | flatten | map(text + {databaseId:.id,createdAt:.created_at}) | {totalCount:length,nodes:.})}}}}
